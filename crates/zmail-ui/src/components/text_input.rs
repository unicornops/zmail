//! Text input component: single-line, multi-line and masked (password)
//! fields with IME support, grapheme-safe editing, mouse and keyboard
//! selection, clipboard actions and undo/redo.
//!
//! Ported from GPUI's `examples/input.rs` in Zed v1.24.2
//! (<https://github.com/zed-industries/zed/blob/v1.24.2/crates/gpui/examples/input.rs>),
//! Copyright Zed Industries, Inc., licensed under the Apache License 2.0.
//! The port adds wrapping and vertical movement for multi-line text, word
//! movement, undo/redo, masking, scrolling, a disabled state and theming.
//!
//! # Usage
//!
//! ```ignore
//! let input = cx.new(|cx| TextInput::new(cx).placeholder("Subject"));
//! cx.subscribe(&input, |this, input, event, cx| match event {
//!     TextInputEvent::Changed => { /* read input.read(cx).text() */ }
//!     TextInputEvent::Submitted => { /* Enter in a single-line field */ }
//! })
//! .detach();
//! ```
//!
//! Every input owns its [`FocusHandle`] and only receives keys while it is
//! focused. Keys reach it as GPUI actions bound in the [`KEY_CONTEXT`] key
//! context (see [`key_bindings`]), so key repeat works and `secondary-`
//! shortcuts map to Cmd on macOS and Ctrl elsewhere. [`init`] must run once
//! per app to install the bindings.

use std::{
    ops::Range,
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Context, CursorStyle, ElementId, ElementInputHandler,
    Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, Hsla,
    KeyBinding, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad,
    Pixels, Point, Role, ScrollWheelEvent, SharedString, Style, TextAlign, TextRun, UTF16Selection,
    UnderlineStyle, Window, WrappedLine, actions, div, fill, point, prelude::*, px, relative, rgb,
    size, transparent_black,
};
use unicode_segmentation::{GraphemeCursor, UnicodeSegmentation};

use super::{ButtonSize, IconButton};
use crate::a11y::{A11yFlags, AccessibleExt};
use crate::styles::{ICON_SIZE_SM, TOOLBAR_BUTTON_HEIGHT, UiColors, rems_from_px};
use crate::widgets::icon;

#[cfg(test)]
mod tests;

/// The key context every text input sets while it is focused.
pub(crate) const KEY_CONTEXT: &str = "TextInput";

actions!(
    text_input,
    [
        /// Delete the selection, or the grapheme before the cursor.
        Backspace,
        /// Delete the selection, or the grapheme after the cursor.
        Delete,
        /// Delete back to the start of the previous word.
        DeleteWordLeft,
        /// Delete forward to the end of the next word.
        DeleteWordRight,
        /// Move the cursor one grapheme left.
        Left,
        /// Move the cursor one grapheme right.
        Right,
        /// Move the cursor up one line (multi-line inputs).
        Up,
        /// Move the cursor down one line (multi-line inputs).
        Down,
        /// Extend the selection one grapheme left.
        SelectLeft,
        /// Extend the selection one grapheme right.
        SelectRight,
        /// Extend the selection up one line.
        SelectUp,
        /// Extend the selection down one line.
        SelectDown,
        /// Move the cursor to the start of the previous word.
        WordLeft,
        /// Move the cursor to the end of the next word.
        WordRight,
        /// Extend the selection to the start of the previous word.
        SelectWordLeft,
        /// Extend the selection to the end of the next word.
        SelectWordRight,
        /// Move the cursor to the start of the line.
        Home,
        /// Move the cursor to the end of the line.
        End,
        /// Extend the selection to the start of the line.
        SelectHome,
        /// Extend the selection to the end of the line.
        SelectEnd,
        /// Move the cursor to the start of the text.
        MoveToStart,
        /// Move the cursor to the end of the text.
        MoveToEnd,
        /// Extend the selection to the start of the text.
        SelectToStart,
        /// Extend the selection to the end of the text.
        SelectToEnd,
        /// Select all text.
        SelectAll,
        /// Copy the selection to the clipboard.
        Copy,
        /// Cut the selection to the clipboard.
        Cut,
        /// Paste the clipboard text.
        Paste,
        /// Undo the last edit.
        Undo,
        /// Redo the last undone edit.
        Redo,
        /// Submit a single-line input, or insert a newline in a multi-line
        /// one.
        Enter,
        /// Move focus to the next field.
        FocusNext,
        /// Move focus to the previous field.
        FocusPrevious,
        /// Open the system character palette (macOS).
        ShowCharacterPalette,
    ]
);

/// Installs the text input key bindings on their own, for component tests.
/// The app binds them with the rest through [`crate::keymap::bind_keymap`].
#[cfg(test)]
pub(crate) fn init(cx: &mut App) {
    cx.bind_keys(key_bindings());
}

/// The default text input key bindings for the current platform, all in
/// the [`KEY_CONTEXT`] context.
pub(crate) fn key_bindings() -> Vec<KeyBinding> {
    let context = Some(KEY_CONTEXT);
    let mut bindings = vec![
        KeyBinding::new("backspace", Backspace, context),
        KeyBinding::new("shift-backspace", Backspace, context),
        KeyBinding::new("delete", Delete, context),
        KeyBinding::new("left", Left, context),
        KeyBinding::new("right", Right, context),
        KeyBinding::new("up", Up, context),
        KeyBinding::new("down", Down, context),
        KeyBinding::new("shift-left", SelectLeft, context),
        KeyBinding::new("shift-right", SelectRight, context),
        KeyBinding::new("shift-up", SelectUp, context),
        KeyBinding::new("shift-down", SelectDown, context),
        KeyBinding::new("home", Home, context),
        KeyBinding::new("end", End, context),
        KeyBinding::new("shift-home", SelectHome, context),
        KeyBinding::new("shift-end", SelectEnd, context),
        KeyBinding::new("enter", Enter, context),
        KeyBinding::new("shift-enter", Enter, context),
        KeyBinding::new("tab", FocusNext, context),
        KeyBinding::new("shift-tab", FocusPrevious, context),
        KeyBinding::new("secondary-a", SelectAll, context),
        KeyBinding::new("secondary-c", Copy, context),
        KeyBinding::new("secondary-x", Cut, context),
        KeyBinding::new("secondary-v", Paste, context),
        KeyBinding::new("secondary-z", Undo, context),
        KeyBinding::new("secondary-shift-z", Redo, context),
    ];
    if cfg!(target_os = "macos") {
        bindings.extend([
            KeyBinding::new("alt-left", WordLeft, context),
            KeyBinding::new("alt-right", WordRight, context),
            KeyBinding::new("alt-shift-left", SelectWordLeft, context),
            KeyBinding::new("alt-shift-right", SelectWordRight, context),
            KeyBinding::new("alt-backspace", DeleteWordLeft, context),
            KeyBinding::new("alt-delete", DeleteWordRight, context),
            KeyBinding::new("cmd-left", Home, context),
            KeyBinding::new("cmd-right", End, context),
            KeyBinding::new("cmd-shift-left", SelectHome, context),
            KeyBinding::new("cmd-shift-right", SelectEnd, context),
            KeyBinding::new("cmd-up", MoveToStart, context),
            KeyBinding::new("cmd-down", MoveToEnd, context),
            KeyBinding::new("cmd-shift-up", SelectToStart, context),
            KeyBinding::new("cmd-shift-down", SelectToEnd, context),
            KeyBinding::new("ctrl-a", Home, context),
            KeyBinding::new("ctrl-e", End, context),
            KeyBinding::new("ctrl-cmd-space", ShowCharacterPalette, context),
        ]);
    } else {
        bindings.extend([
            KeyBinding::new("ctrl-left", WordLeft, context),
            KeyBinding::new("ctrl-right", WordRight, context),
            KeyBinding::new("ctrl-shift-left", SelectWordLeft, context),
            KeyBinding::new("ctrl-shift-right", SelectWordRight, context),
            KeyBinding::new("ctrl-backspace", DeleteWordLeft, context),
            KeyBinding::new("ctrl-delete", DeleteWordRight, context),
            KeyBinding::new("ctrl-home", MoveToStart, context),
            KeyBinding::new("ctrl-end", MoveToEnd, context),
            KeyBinding::new("ctrl-shift-home", SelectToStart, context),
            KeyBinding::new("ctrl-shift-end", SelectToEnd, context),
            KeyBinding::new("ctrl-y", Redo, context),
            KeyBinding::new("ctrl-insert", Copy, context),
            KeyBinding::new("shift-insert", Paste, context),
        ]);
    }
    bindings
}

/// Events a [`TextInput`] emits to its subscribers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextInputEvent {
    /// The user changed the text (typing, deleting, pasting, IME, undo).
    /// Programmatic [`TextInput::set_text`] calls do not emit it.
    Changed,
    /// The user pressed Enter in a single-line input.
    Submitted,
}

/// Character drawn for each grapheme of a masked input.
const MASK_CHAR: char = '\u{2022}';
/// Consecutive edits of one kind closer together than this undo together.
const UNDO_GROUP_INTERVAL: Duration = Duration::from_millis(1000);
/// Oldest undo steps are dropped beyond this depth.
const MAX_UNDO_DEPTH: usize = 1000;
/// Width of the text cursor.
const CURSOR_WIDTH: Pixels = px(2.);
/// Default visible rows of a multi-line input.
const DEFAULT_MIN_ROWS: usize = 3;
const DEFAULT_MAX_ROWS: usize = 12;

/// An editable text field. Create it with `cx.new(|cx| TextInput::new(cx))`
/// and configure it with the builder methods.
pub(crate) struct TextInput {
    focus_handle: FocusHandle,
    content: String,
    placeholder: SharedString,
    label: Option<SharedString>,
    leading_icon: Option<&'static str>,
    /// The accessible name of the clear button a non-empty field shows,
    /// when it has one.
    clear_label: Option<SharedString>,
    multiline: bool,
    masked: bool,
    disabled: bool,
    compact: bool,
    borderless: bool,
    min_rows: usize,
    max_rows: usize,
    /// Selected byte range of `content`, always on grapheme boundaries
    /// after user movement and on char boundaries in every case.
    selected_range: Range<usize>,
    /// The cursor is at the start of `selected_range`, not the end.
    selection_reversed: bool,
    /// IME composition range (bytes of `content`).
    marked_range: Option<Range<usize>>,
    /// Horizontal position kept across consecutive Up/Down moves.
    goal_x: Option<Pixels>,
    /// A mouse drag selection is in progress.
    is_selecting: bool,
    /// Text layout from the last paint, used for hit testing, vertical
    /// movement and IME candidate placement.
    layout: Option<Rc<InputLayout>>,
    /// Scroll offset of the text inside the field: horizontal for
    /// single-line inputs, vertical for multi-line ones.
    scroll_offset: Point<Pixels>,
    /// Scroll the cursor into view on the next paint.
    autoscroll: bool,
    history: History,
}

impl EventEmitter<TextInputEvent> for TextInput {}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

// ---------------------------------------------------------------------------
// Construction and public API
// ---------------------------------------------------------------------------

impl TextInput {
    /// An empty, enabled, single-line input.
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle().tab_stop(true),
            content: String::new(),
            placeholder: SharedString::default(),
            label: None,
            leading_icon: None,
            clear_label: None,
            multiline: false,
            masked: false,
            disabled: false,
            compact: false,
            borderless: false,
            min_rows: DEFAULT_MIN_ROWS,
            max_rows: DEFAULT_MAX_ROWS,
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            goal_x: None,
            is_selecting: false,
            layout: None,
            scroll_offset: Point::default(),
            autoscroll: false,
            history: History::default(),
        }
    }

    /// Starts with `text`, the cursor at its end.
    pub(crate) fn with_text(mut self, text: impl Into<String>) -> Self {
        self.content = self.sanitize(&text.into());
        let end = self.content.len();
        self.selected_range = end..end;
        self
    }

    /// Text shown while the input is empty.
    pub(crate) fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// The accessible name announced by screen readers (usually the visible
    /// field label).
    pub(crate) fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// An icon drawn before the text, for example `"search"`.
    pub(crate) fn leading_icon(mut self, name: &'static str) -> Self {
        self.leading_icon = Some(name);
        self
    }

    /// A clear button at the end of a non-empty field, named `label`.
    /// Clicking it empties the field as one undoable edit, emits
    /// [`TextInputEvent::Changed`] and leaves focus in the field.
    pub(crate) fn clearable(mut self, label: impl Into<SharedString>) -> Self {
        self.clear_label = Some(label.into());
        self
    }

    /// Multi-line: Enter inserts a newline, text wraps, Up/Down move by
    /// line and the field scrolls once it reaches its maximum height.
    pub(crate) fn multiline(mut self) -> Self {
        self.multiline = true;
        self.content = self.sanitize(&self.content);
        self
    }

    /// The visible height of a multi-line input, in rows: it grows with
    /// its content from `min` to `max` rows, then scrolls.
    pub(crate) fn rows(mut self, min: usize, max: usize) -> Self {
        self.min_rows = min.max(1);
        self.max_rows = max.max(self.min_rows);
        self
    }

    /// Masked (password) input: each character draws as a bullet, and copy
    /// and cut are disabled.
    pub(crate) fn masked(mut self) -> Self {
        self.masked = true;
        self
    }

    /// A shorter field for toolbars.
    pub(crate) fn compact(mut self) -> Self {
        self.compact = true;
        self
    }

    /// Draw only the text, with no border, fill or focus ring, for a field
    /// embedded in a larger control that draws its own (recipient chips).
    pub(crate) fn borderless(mut self) -> Self {
        self.borderless = true;
        self
    }

    /// The current text.
    pub(crate) fn text(&self) -> &str {
        &self.content
    }

    /// Replaces the text programmatically, puts the cursor at the end and
    /// clears the undo history. Does not emit [`TextInputEvent::Changed`].
    pub(crate) fn set_text(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.content = self.sanitize(&text.into());
        let end = self.content.len();
        self.selected_range = end..end;
        self.selection_reversed = false;
        self.marked_range = None;
        self.goal_x = None;
        self.history = History::default();
        self.autoscroll = true;
        cx.notify();
    }

    /// Enables or disables editing. A disabled input cannot be focused or
    /// edited.
    // Component API that no view uses yet.
    #[allow(dead_code)]
    pub(crate) fn set_disabled(
        &mut self,
        disabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.disabled = disabled;
        if disabled && self.focus_handle.is_focused(window) {
            window.blur(cx);
        }
        cx.notify();
    }

    /// Whether editing is disabled.
    // Component API that no view uses yet.
    #[allow(dead_code)]
    pub(crate) fn is_disabled(&self) -> bool {
        self.disabled
    }

    /// The selected byte range of [`Self::text`] (empty when there is only
    /// a cursor).
    // Component API that no view uses yet.
    #[allow(dead_code)]
    pub(crate) fn selected_range(&self) -> Range<usize> {
        self.selected_range.clone()
    }

    /// The cursor position as a byte offset into [`Self::text`].
    pub(crate) fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    /// Selects `range` (byte offsets, snapped to grapheme boundaries).
    pub(crate) fn select_range(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        let start = self.snap_to_grapheme(range.start);
        let end = self.snap_to_grapheme(range.end);
        self.selection_reversed = end < start;
        self.selected_range = start.min(end)..start.max(end);
        self.after_selection_change(cx);
    }

    /// Moves keyboard focus to this input.
    pub(crate) fn focus(&self, window: &mut Window, cx: &mut App) {
        if !self.disabled {
            window.focus(&self.focus_handle, cx);
        }
    }

    /// Empties the field as the user would (one undoable edit that emits
    /// [`TextInputEvent::Changed`]), unlike [`Self::set_text`].
    pub(crate) fn clear(&mut self, cx: &mut Context<Self>) {
        self.marked_range = None;
        self.edit_and_place_cursor(0..self.content.len(), "", EditKind::Other, cx);
    }
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

impl TextInput {
    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx);
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.end, cx);
        }
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        if !self.multiline {
            // Single-line inputs leave Up/Down to the surrounding view (for
            // example list navigation from a search field).
            cx.propagate();
            return;
        }
        let (offset, goal) = self.vertical_target(-1);
        self.move_to(offset, cx);
        self.goal_x = goal;
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        if !self.multiline {
            cx.propagate();
            return;
        }
        let (offset, goal) = self.vertical_target(1);
        self.move_to(offset, cx);
        self.goal_x = goal;
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        if !self.multiline {
            self.select_to(0, cx);
            return;
        }
        let (offset, goal) = self.vertical_target(-1);
        self.select_to(offset, cx);
        self.goal_x = goal;
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        if !self.multiline {
            self.select_to(self.content.len(), cx);
            return;
        }
        let (offset, goal) = self.vertical_target(1);
        self.select_to(offset, cx);
        self.goal_x = goal;
    }

    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.previous_word_start(self.cursor_offset()), cx);
    }

    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.next_word_end(self.cursor_offset()), cx);
    }

    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_word_start(self.cursor_offset()), cx);
    }

    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_word_end(self.cursor_offset()), cx);
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.line_start(self.cursor_offset()), cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.line_end(self.cursor_offset()), cx);
    }

    fn select_home(&mut self, _: &SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.line_start(self.cursor_offset()), cx);
    }

    fn select_end(&mut self, _: &SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.line_end(self.cursor_offset()), cx);
    }

    fn move_to_start(&mut self, _: &MoveToStart, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn move_to_end(&mut self, _: &MoveToEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn select_to_start(&mut self, _: &SelectToStart, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(0, cx);
    }

    fn select_to_end(&mut self, _: &SelectToEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.content.len(), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.selected_range = 0..self.content.len();
        self.selection_reversed = false;
        self.after_selection_change(cx);
    }

    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            self.replace_selection("", EditKind::Other, cx);
            return;
        }
        let cursor = self.cursor_offset();
        let previous = self.previous_boundary(cursor);
        if previous < cursor {
            self.edit_and_place_cursor(previous..cursor, "", EditKind::Deleting, cx);
        }
    }

    fn delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            self.replace_selection("", EditKind::Other, cx);
            return;
        }
        let cursor = self.cursor_offset();
        let next = self.next_boundary(cursor);
        if next > cursor {
            self.edit_and_place_cursor(cursor..next, "", EditKind::Deleting, cx);
        }
    }

    fn delete_word_left(&mut self, _: &DeleteWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            self.replace_selection("", EditKind::Other, cx);
            return;
        }
        let cursor = self.cursor_offset();
        let start = self.previous_word_start(cursor);
        if start < cursor {
            self.edit_and_place_cursor(start..cursor, "", EditKind::Other, cx);
        }
    }

    fn delete_word_right(&mut self, _: &DeleteWordRight, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            self.replace_selection("", EditKind::Other, cx);
            return;
        }
        let cursor = self.cursor_offset();
        let end = self.next_word_end(cursor);
        if end > cursor {
            self.edit_and_place_cursor(cursor..end, "", EditKind::Other, cx);
        }
    }

    fn enter(&mut self, _: &Enter, _: &mut Window, cx: &mut Context<Self>) {
        if self.multiline {
            self.replace_selection("\n", EditKind::Typing, cx);
        } else {
            cx.emit(TextInputEvent::Submitted);
        }
    }

    fn focus_next(&mut self, _: &FocusNext, window: &mut Window, cx: &mut Context<Self>) {
        window.focus_next(cx);
    }

    fn focus_previous(&mut self, _: &FocusPrevious, window: &mut Window, cx: &mut Context<Self>) {
        window.focus_prev(cx);
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if self.masked || self.selected_range.is_empty() {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(
            self.content[self.selected_range.clone()].to_owned(),
        ));
    }

    fn cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
        if self.masked || self.selected_range.is_empty() {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(
            self.content[self.selected_range.clone()].to_owned(),
        ));
        self.replace_selection("", EditKind::Other, cx);
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        // The clipboard is read only when the user actually pastes.
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_selection(&text, EditKind::Other, cx);
        }
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        let Some(transaction) = self.history.undo_stack.pop() else {
            return;
        };
        let range = transaction.start..transaction.start + transaction.new_text.len();
        if !self.is_valid_range(&range) {
            self.history = History::default();
            return;
        }
        self.content.replace_range(range, &transaction.old_text);
        self.selected_range = self.clamp_range(transaction.selection_before.clone());
        self.selection_reversed = transaction.reversed_before;
        self.marked_range = None;
        self.history.redo_stack.push(transaction);
        self.history.break_group();
        self.after_content_change(cx);
    }

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        let Some(transaction) = self.history.redo_stack.pop() else {
            return;
        };
        let range = transaction.start..transaction.start + transaction.old_text.len();
        if !self.is_valid_range(&range) {
            self.history = History::default();
            return;
        }
        self.content.replace_range(range, &transaction.new_text);
        let cursor = transaction.start + transaction.new_text.len();
        self.selected_range = cursor..cursor;
        self.selection_reversed = false;
        self.marked_range = None;
        self.history.undo_stack.push(transaction);
        self.history.break_group();
        self.after_content_change(cx);
    }

    fn show_character_palette(
        &mut self,
        _: &ShowCharacterPalette,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.show_character_palette();
    }

    // -----------------------------------------------------------------------
    // Mouse
    // -----------------------------------------------------------------------

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.disabled {
            return;
        }
        window.focus(&self.focus_handle, cx);
        let offset = self.index_for_mouse_position(event.position);
        match event.click_count {
            2 => {
                let word = self.word_range_at(offset);
                self.selected_range = word;
                self.selection_reversed = false;
                self.after_selection_change(cx);
            }
            count if count >= 3 => {
                let range = if self.multiline {
                    self.logical_line_range(offset)
                } else {
                    0..self.content.len()
                };
                self.selected_range = range;
                self.selection_reversed = false;
                self.after_selection_change(cx);
            }
            _ => {
                self.is_selecting = true;
                if event.modifiers.shift {
                    self.select_to(offset, cx);
                } else {
                    self.move_to(offset, cx);
                }
            }
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting && event.pressed_button == Some(MouseButton::Left) {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        } else if self.is_selecting {
            self.is_selecting = false;
        }
    }

    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.multiline {
            return;
        }
        let Some(layout) = self.layout.clone() else {
            return;
        };
        let max_scroll = (layout.content_height() - layout.bounds.size.height).max(px(0.));
        if max_scroll <= px(0.) {
            // Nothing to scroll: let the surrounding pane scroll instead.
            cx.propagate();
            return;
        }
        let delta = event.delta.pixel_delta(layout.line_height);
        let scrolled = (self.scroll_offset.y - delta.y).clamp(px(0.), max_scroll);
        if scrolled != self.scroll_offset.y {
            self.scroll_offset.y = scrolled;
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        let Some(layout) = self.layout.as_ref() else {
            return self.content.len();
        };
        let display = layout.offset_for_position(position - layout.text_origin);
        self.snap_to_grapheme(self.display_to_content(display))
    }
}

// ---------------------------------------------------------------------------
// Editing and movement internals
// ---------------------------------------------------------------------------

impl TextInput {
    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = self.clamp_offset(offset);
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        self.after_selection_change(cx);
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        let offset = self.clamp_offset(offset);
        if self.selection_reversed {
            self.selected_range.start = offset;
        } else {
            self.selected_range.end = offset;
        }
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        self.after_selection_change(cx);
    }

    fn after_selection_change(&mut self, cx: &mut Context<Self>) {
        self.goal_x = None;
        self.autoscroll = true;
        self.history.break_group();
        cx.notify();
    }

    fn after_content_change(&mut self, cx: &mut Context<Self>) {
        self.goal_x = None;
        self.autoscroll = true;
        cx.emit(TextInputEvent::Changed);
        cx.notify();
    }

    /// Replaces the selection (or the IME composition) with `text` and puts
    /// the cursor after it.
    fn replace_selection(&mut self, text: &str, kind: EditKind, cx: &mut Context<Self>) {
        let range = self
            .marked_range
            .clone()
            .unwrap_or_else(|| self.selected_range.clone());
        self.edit_and_place_cursor(range, text, kind, cx);
    }

    fn edit_and_place_cursor(
        &mut self,
        range: Range<usize>,
        text: &str,
        kind: EditKind,
        cx: &mut Context<Self>,
    ) {
        if self.disabled {
            return;
        }
        let Some(inserted) = self.apply_edit(range, text, kind, cx) else {
            return;
        };
        self.selected_range = inserted.end..inserted.end;
        self.selection_reversed = false;
        self.marked_range = None;
        self.after_content_change(cx);
    }

    /// Replaces `range` with the sanitized `text`, records the edit for
    /// undo, and returns the inserted range. Returns `None` when nothing
    /// changed. Selection, marked text and events are left to the caller.
    fn apply_edit(
        &mut self,
        range: Range<usize>,
        text: &str,
        kind: EditKind,
        cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        let range = self.clamp_range(range);
        let text = self.sanitize(text);
        if range.is_empty() && text.is_empty() {
            return None;
        }
        let old_text = self.content[range.clone()].to_owned();
        self.content.replace_range(range.clone(), &text);
        let now = cx.background_executor().now();
        self.history.record(
            Transaction {
                start: range.start,
                old_text,
                new_text: text.clone(),
                selection_before: self.selected_range.clone(),
                reversed_before: self.selection_reversed,
            },
            kind,
            now,
        );
        Some(range.start..range.start + text.len())
    }

    /// Single-line inputs turn newlines and tabs into spaces; multi-line
    /// inputs normalise line endings to `\n`.
    fn sanitize(&self, text: &str) -> String {
        if self.multiline {
            text.replace("\r\n", "\n").replace('\r', "\n")
        } else {
            text.replace("\r\n", " ").replace(['\n', '\r', '\t'], " ")
        }
    }

    fn is_valid_range(&self, range: &Range<usize>) -> bool {
        range.start <= range.end
            && range.end <= self.content.len()
            && self.content.is_char_boundary(range.start)
            && self.content.is_char_boundary(range.end)
    }

    fn clamp_offset(&self, offset: usize) -> usize {
        let mut offset = offset.min(self.content.len());
        while !self.content.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    }

    fn clamp_range(&self, range: Range<usize>) -> Range<usize> {
        let start = self.clamp_offset(range.start);
        let end = self.clamp_offset(range.end);
        start.min(end)..start.max(end)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        let offset = self.clamp_offset(offset);
        let mut cursor = GraphemeCursor::new(offset, self.content.len(), true);
        cursor
            .prev_boundary(&self.content, 0)
            .ok()
            .flatten()
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        let offset = self.clamp_offset(offset);
        let mut cursor = GraphemeCursor::new(offset, self.content.len(), true);
        cursor
            .next_boundary(&self.content, 0)
            .ok()
            .flatten()
            .unwrap_or(self.content.len())
    }

    /// The nearest grapheme boundary at or before `offset`.
    fn snap_to_grapheme(&self, offset: usize) -> usize {
        let offset = self.clamp_offset(offset);
        let mut cursor = GraphemeCursor::new(offset, self.content.len(), true);
        match cursor.is_boundary(&self.content, 0) {
            Ok(true) => offset,
            _ => self.previous_boundary(offset),
        }
    }

    fn previous_word_start(&self, offset: usize) -> usize {
        if self.masked {
            return 0;
        }
        let mut start = 0;
        for (index, word) in self.content.split_word_bound_indices() {
            if index >= offset {
                break;
            }
            if is_word(word) {
                start = index;
            }
        }
        start
    }

    fn next_word_end(&self, offset: usize) -> usize {
        if self.masked {
            return self.content.len();
        }
        self.content
            .split_word_bound_indices()
            .map(|(index, word)| (index + word.len(), word))
            .find(|(end, word)| *end > offset && is_word(word))
            .map(|(end, _)| end)
            .unwrap_or(self.content.len())
    }

    /// The word (or run of whitespace or punctuation) containing `offset`.
    fn word_range_at(&self, offset: usize) -> Range<usize> {
        if self.masked {
            return 0..self.content.len();
        }
        self.content
            .split_word_bound_indices()
            .map(|(index, word)| index..index + word.len())
            .find(|range| range.contains(&offset))
            .or_else(|| {
                self.content
                    .split_word_bound_indices()
                    .next_back()
                    .map(|(index, word)| index..index + word.len())
            })
            .unwrap_or(offset..offset)
    }

    /// The `\n`-delimited line containing `offset`, without its newline.
    fn logical_line_range(&self, offset: usize) -> Range<usize> {
        let offset = self.clamp_offset(offset);
        let start = self.content[..offset]
            .rfind('\n')
            .map_or(0, |index| index + 1);
        let end = self.content[offset..]
            .find('\n')
            .map_or(self.content.len(), |index| offset + index);
        start..end
    }

    /// Start of the visual row containing `offset` (the whole text for a
    /// single-line input).
    fn line_start(&self, offset: usize) -> usize {
        if !self.multiline {
            return 0;
        }
        match self.visual_row_edge(offset, false) {
            Some(start) => start,
            None => self.logical_line_range(offset).start,
        }
    }

    /// End of the visual row containing `offset`.
    fn line_end(&self, offset: usize) -> usize {
        if !self.multiline {
            return self.content.len();
        }
        match self.visual_row_edge(offset, true) {
            Some(end) => end,
            None => self.logical_line_range(offset).end,
        }
    }

    fn visual_row_edge(&self, offset: usize, end: bool) -> Option<usize> {
        if self.content.is_empty() {
            return Some(0);
        }
        let layout = self
            .layout
            .as_ref()
            .filter(|layout| !layout.is_placeholder)?;
        let position = layout.position_for_offset(self.content_to_display(offset));
        let x = if end { px(f32::MAX / 4.) } else { px(0.) };
        let y = position.y + layout.line_height / 2.;
        let display = layout.offset_for_position(point(x, y));
        Some(self.snap_to_grapheme(self.display_to_content(display)))
    }

    /// The offset one row above (`-1`) or below (`1`) the cursor, keeping
    /// the goal column, and the goal column to keep.
    fn vertical_target(&self, direction: i32) -> (usize, Option<Pixels>) {
        let cursor = self.cursor_offset();
        if let Some(layout) = self.layout.as_ref().filter(|layout| !layout.is_placeholder) {
            let position = layout.position_for_offset(self.content_to_display(cursor));
            let x = self.goal_x.unwrap_or(position.x);
            let y = position.y + layout.line_height * direction as f32 + layout.line_height / 2.;
            if y < px(0.) {
                return (0, Some(x));
            }
            if y >= layout.content_height() {
                return (self.content.len(), Some(x));
            }
            let display = layout.offset_for_position(point(x, y));
            return (
                self.snap_to_grapheme(self.display_to_content(display)),
                Some(x),
            );
        }

        // Not painted yet: move by logical line, keeping the grapheme
        // column.
        let line = self.logical_line_range(cursor);
        let column = self.content[line.start..cursor].graphemes(true).count();
        let target = if direction < 0 {
            if line.start == 0 {
                return (0, None);
            }
            self.logical_line_range(line.start - 1)
        } else {
            if line.end >= self.content.len() {
                return (self.content.len(), None);
            }
            self.logical_line_range(line.end + 1)
        };
        let offset = self.content[target.clone()]
            .grapheme_indices(true)
            .nth(column)
            .map_or(target.end, |(index, _)| target.start + index);
        (offset, None)
    }

    // -----------------------------------------------------------------------
    // Display mapping (masking) and UTF-16 conversion
    // -----------------------------------------------------------------------

    fn display_text(&self) -> String {
        if self.masked {
            MASK_CHAR
                .to_string()
                .repeat(self.content.graphemes(true).count())
        } else {
            self.content.clone()
        }
    }

    fn content_to_display(&self, offset: usize) -> usize {
        if !self.masked {
            return offset;
        }
        let graphemes = self
            .content
            .grapheme_indices(true)
            .take_while(|(index, _)| *index < offset)
            .count();
        graphemes * MASK_CHAR.len_utf8()
    }

    fn display_to_content(&self, offset: usize) -> usize {
        if !self.masked {
            return self.clamp_offset(offset);
        }
        self.content
            .grapheme_indices(true)
            .nth(offset / MASK_CHAR.len_utf8())
            .map_or(self.content.len(), |(index, _)| index)
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }
}

/// Whether a word-boundary segment is a word (not whitespace or
/// punctuation).
fn is_word(segment: &str) -> bool {
    segment.chars().any(char::is_alphanumeric)
}

/// Byte offset of UTF-16 offset `offset` within `text`.
fn utf16_to_utf8_in(text: &str, offset: usize) -> usize {
    let mut utf8 = 0;
    let mut utf16 = 0;
    for ch in text.chars() {
        if utf16 >= offset {
            break;
        }
        utf16 += ch.len_utf16();
        utf8 += ch.len_utf8();
    }
    utf8
}

// ---------------------------------------------------------------------------
// IME
// ---------------------------------------------------------------------------

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.clamp_range(self.range_from_utf16(&range_utf16));
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.marked_range = None;
        cx.notify();
    }

    fn paste(&mut self, item: ClipboardItem, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = item.text() {
            self.replace_selection(&text, EditKind::Other, cx);
        }
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let kind = if self.marked_range.is_some() {
            EditKind::Composing
        } else {
            EditKind::Typing
        };
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or_else(|| self.selected_range.clone());
        if self.marked_range.is_some() && range.is_empty() && new_text.is_empty() {
            // Committing an empty composition just ends it.
            self.marked_range = None;
            cx.notify();
            return;
        }
        self.edit_and_place_cursor(range, new_text, kind, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.disabled {
            return;
        }
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or_else(|| self.selected_range.clone());
        let range = self.clamp_range(range);
        let inserted = self
            .apply_edit(range.clone(), new_text, EditKind::Composing, cx)
            .unwrap_or(range.start..range.start);
        self.marked_range = (!inserted.is_empty()).then(|| inserted.clone());
        let inserted_text = self.content[inserted.clone()].to_owned();
        self.selected_range = new_selected_range_utf16
            .map(|selected| {
                let start = inserted.start + utf16_to_utf8_in(&inserted_text, selected.start);
                let end = inserted.start + utf16_to_utf8_in(&inserted_text, selected.end);
                start..end
            })
            .unwrap_or(inserted.end..inserted.end);
        self.selection_reversed = false;
        self.after_content_change(cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.layout.as_ref()?;
        let range = self.clamp_range(self.range_from_utf16(&range_utf16));
        let start = layout.position_for_offset(self.content_to_display(range.start));
        let end = layout.position_for_offset(self.content_to_display(range.end));
        let width = if end.y == start.y {
            (end.x - start.x).max(CURSOR_WIDTH)
        } else {
            CURSOR_WIDTH
        };
        Some(Bounds::new(
            layout.text_origin + start,
            size(width, layout.line_height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let layout = self.layout.as_ref()?;
        if !layout.bounds.contains(&point) {
            return None;
        }
        let display = layout.offset_for_position(point - layout.text_origin);
        Some(self.offset_to_utf16(self.display_to_content(display)))
    }

    fn text_length_utf16(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.offset_to_utf16(self.content.len()))
    }

    fn accepts_text_input(&self, _window: &mut Window, _cx: &mut Context<Self>) -> bool {
        !self.disabled
    }
}

// ---------------------------------------------------------------------------
// Undo history
// ---------------------------------------------------------------------------

/// What kind of edit a transaction was, for undo grouping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditKind {
    /// Typed text; consecutive typing undoes a word at a time.
    Typing,
    /// Single-grapheme Backspace or Delete.
    Deleting,
    /// IME composition and its commit.
    Composing,
    /// Paste, cut, selection deletes and word deletes: always their own
    /// undo step.
    Other,
}

/// One undoable edit: `old_text` at `start` was replaced by `new_text`.
#[derive(Debug, Clone)]
struct Transaction {
    start: usize,
    old_text: String,
    new_text: String,
    selection_before: Range<usize>,
    reversed_before: bool,
}

impl Transaction {
    /// Folds `next` into this transaction when it continues the same
    /// gesture. Returns `false` when it must be its own undo step.
    fn try_merge(&mut self, next: &Transaction, kind: EditKind) -> bool {
        let new_end = self.start + self.new_text.len();
        match kind {
            EditKind::Typing => {
                if !next.old_text.is_empty() || next.start != new_end {
                    return false;
                }
                // Each word (with the whitespace after it) is one step.
                let previous_whitespace = self
                    .new_text
                    .chars()
                    .last()
                    .is_some_and(char::is_whitespace);
                let next_whitespace = next
                    .new_text
                    .chars()
                    .next()
                    .is_some_and(char::is_whitespace);
                if previous_whitespace && !next_whitespace {
                    return false;
                }
                self.new_text.push_str(&next.new_text);
                true
            }
            EditKind::Deleting => {
                if !next.new_text.is_empty() || !self.new_text.is_empty() {
                    return false;
                }
                if next.start + next.old_text.len() == self.start {
                    // Backspace: the deleted text precedes what was deleted
                    // before.
                    self.old_text.insert_str(0, &next.old_text);
                    self.start = next.start;
                    true
                } else if next.start == self.start {
                    // Forward delete.
                    self.old_text.push_str(&next.old_text);
                    true
                } else {
                    false
                }
            }
            EditKind::Composing => {
                let next_end = next.start + next.old_text.len();
                if next.start < self.start || next_end > new_end {
                    return false;
                }
                let relative = next.start - self.start;
                self.new_text
                    .replace_range(relative..relative + next.old_text.len(), &next.new_text);
                true
            }
            EditKind::Other => false,
        }
    }
}

#[derive(Debug, Default)]
struct History {
    undo_stack: Vec<Transaction>,
    redo_stack: Vec<Transaction>,
    /// Kind and time of the last edit while its group is still open.
    open_group: Option<(EditKind, Instant)>,
}

impl History {
    fn record(&mut self, transaction: Transaction, kind: EditKind, now: Instant) {
        self.redo_stack.clear();
        let continues_group = matches!(
            self.open_group,
            Some((last_kind, at))
                if last_kind == kind
                    && kind != EditKind::Other
                    && now.saturating_duration_since(at) <= UNDO_GROUP_INTERVAL
        );
        let merged = continues_group
            && self
                .undo_stack
                .last_mut()
                .is_some_and(|last| last.try_merge(&transaction, kind));
        if !merged {
            self.undo_stack.push(transaction);
            if self.undo_stack.len() > MAX_UNDO_DEPTH {
                self.undo_stack.remove(0);
            }
        }
        self.open_group = (kind != EditKind::Other).then_some((kind, now));
    }

    fn break_group(&mut self) {
        self.open_group = None;
    }
}

// ---------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------

/// The shaped text from the last paint.
struct InputLayout {
    /// One wrapped line per `\n`-separated line of the display text.
    lines: Vec<WrappedLine>,
    /// Display-text byte offset where each line starts.
    line_starts: Vec<usize>,
    line_height: Pixels,
    /// Window position of the text's top-left corner, scroll included.
    text_origin: Point<Pixels>,
    /// The visible text area.
    bounds: Bounds<Pixels>,
    /// The placeholder was shaped instead of the (empty) content.
    is_placeholder: bool,
}

impl InputLayout {
    fn content_height(&self) -> Pixels {
        self.lines
            .iter()
            .map(|line| line.size(self.line_height).height)
            .fold(px(0.), |total, height| total + height)
    }

    fn content_width(&self) -> Pixels {
        self.lines
            .iter()
            .map(|line| line.unwrapped_layout.width)
            .max()
            .unwrap_or_default()
    }

    /// Position of a display offset relative to the text origin.
    fn position_for_offset(&self, offset: usize) -> Point<Pixels> {
        if self.is_placeholder {
            return Point::default();
        }
        let mut top = px(0.);
        for (line, start) in self.lines.iter().zip(&self.line_starts) {
            let end = start + line.len();
            if offset <= end {
                let local = line
                    .position_for_index(offset.saturating_sub(*start), self.line_height)
                    .unwrap_or_default();
                return point(local.x, top + local.y);
            }
            top += line.size(self.line_height).height;
        }
        let last_height = self
            .lines
            .last()
            .map(|line| line.size(self.line_height).height)
            .unwrap_or(self.line_height);
        point(
            self.lines
                .last()
                .map(|line| line.unwrapped_layout.width)
                .unwrap_or_default(),
            (top - last_height).max(px(0.)),
        )
    }

    /// The display offset closest to a position relative to the text
    /// origin.
    fn offset_for_position(&self, position: Point<Pixels>) -> usize {
        if self.is_placeholder {
            return 0;
        }
        let mut top = px(0.);
        let count = self.lines.len();
        for (index, (line, start)) in self.lines.iter().zip(&self.line_starts).enumerate() {
            let height = line.size(self.line_height).height;
            if position.y < top + height || index + 1 == count {
                let local_y = (position.y - top).clamp(px(0.), (height - px(1.)).max(px(0.)));
                let local = point(position.x.max(px(0.)), local_y);
                let index = match line.closest_index_for_position(local, self.line_height) {
                    Ok(index) | Err(index) => index,
                };
                return start + index.min(line.len());
            }
            top += height;
        }
        0
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// The colours the input draws with: the active theme's palette, published
/// by the workspace as a GPUI global, or the bundled dark palette.
fn palette(cx: &App) -> UiColors {
    cx.try_global::<UiColors>()
        .copied()
        .unwrap_or_else(UiColors::fallback)
}

fn with_alpha(color: u32, alpha: f32) -> Hsla {
    let mut color: Hsla = rgb(color).into();
    color.a = alpha;
    color
}

impl Render for TextInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = palette(cx);
        let focused = !self.disabled && self.focus_handle.is_focused(window);
        let role = if self.masked {
            Role::PasswordInput
        } else if self.multiline {
            Role::MultilineTextInput
        } else {
            Role::TextInput
        };
        let field_background = if self.disabled || !focused {
            colors.surface
        } else {
            colors.background
        };
        let border = if focused {
            colors.focus_ring
        } else {
            colors.control_border
        };
        // Unfocused fields draw a transparent ring so focusing never moves
        // the text; focused fields draw a FOCUS_RING_WIDTH outline (1px ring
        // plus the 1px border) in the focus-ring colour.
        let ring = if focused {
            with_alpha(colors.focus_ring, 1.)
        } else {
            transparent_black()
        };
        let text_color = if self.disabled {
            colors.muted
        } else {
            colors.foreground
        };

        div()
            .id(ElementId::NamedInteger(
                "text-input".into(),
                cx.entity_id().as_u64(),
            ))
            // A field without a visible label is named by its placeholder.
            .accessible(
                role,
                self.label
                    .clone()
                    .unwrap_or_else(|| self.placeholder.clone()),
            )
            .when(!self.placeholder.is_empty(), |this| {
                this.aria_placeholder(self.placeholder.clone())
            })
            .when(!self.masked, |this| {
                this.aria_value(SharedString::from(self.content.clone()))
            })
            .a11y_flags(A11yFlags::disabled(self.disabled))
            .key_context(KEY_CONTEXT)
            .when(!self.disabled, |this| {
                this.track_focus(&self.focus_handle)
                    .on_action(cx.listener(Self::backspace))
                    .on_action(cx.listener(Self::delete))
                    .on_action(cx.listener(Self::delete_word_left))
                    .on_action(cx.listener(Self::delete_word_right))
                    .on_action(cx.listener(Self::left))
                    .on_action(cx.listener(Self::right))
                    .on_action(cx.listener(Self::up))
                    .on_action(cx.listener(Self::down))
                    .on_action(cx.listener(Self::select_left))
                    .on_action(cx.listener(Self::select_right))
                    .on_action(cx.listener(Self::select_up))
                    .on_action(cx.listener(Self::select_down))
                    .on_action(cx.listener(Self::word_left))
                    .on_action(cx.listener(Self::word_right))
                    .on_action(cx.listener(Self::select_word_left))
                    .on_action(cx.listener(Self::select_word_right))
                    .on_action(cx.listener(Self::home))
                    .on_action(cx.listener(Self::end))
                    .on_action(cx.listener(Self::select_home))
                    .on_action(cx.listener(Self::select_end))
                    .on_action(cx.listener(Self::move_to_start))
                    .on_action(cx.listener(Self::move_to_end))
                    .on_action(cx.listener(Self::select_to_start))
                    .on_action(cx.listener(Self::select_to_end))
                    .on_action(cx.listener(Self::select_all))
                    .on_action(cx.listener(Self::copy))
                    .on_action(cx.listener(Self::cut))
                    .on_action(cx.listener(Self::paste))
                    .on_action(cx.listener(Self::undo))
                    .on_action(cx.listener(Self::redo))
                    .on_action(cx.listener(Self::enter))
                    .on_action(cx.listener(Self::focus_next))
                    .on_action(cx.listener(Self::focus_previous))
                    .on_action(cx.listener(Self::show_character_palette))
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
                    .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
                    .on_mouse_move(cx.listener(Self::on_mouse_move))
                    .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            })
            .w_full()
            .min_w_0()
            .when(!self.borderless, |this| {
                this.p(px(1.)).rounded_lg().bg(ring)
            })
            .child(
                div()
                    .flex()
                    .when(self.multiline, |this| this.items_start())
                    .when(!self.multiline, |this| this.items_center())
                    .gap_2()
                    .w_full()
                    .min_w_0()
                    .when(self.compact, |this| {
                        // The ring adds 1px on each side.
                        this.h(rems_from_px(TOOLBAR_BUTTON_HEIGHT - 2.))
                    })
                    .when(!self.compact, |this| this.py_2())
                    .when(self.borderless, |this| this.px_1())
                    .when(!self.borderless, |this| {
                        this.px_3()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(border))
                            .bg(rgb(field_background))
                    })
                    .text_color(rgb(text_color))
                    .when(self.disabled, |this| this.opacity(0.6))
                    .cursor(if self.disabled {
                        CursorStyle::Arrow
                    } else {
                        CursorStyle::IBeam
                    })
                    .when_some(self.leading_icon, |this, name| {
                        this.child(icon(name, ICON_SIZE_SM, colors.muted))
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(TextElement { input: cx.entity() }),
                    )
                    .when_some(
                        self.clear_label
                            .clone()
                            .filter(|_| !self.disabled && !self.content.is_empty()),
                        |this, label| this.child(self.render_clear_button(label, cx)),
                    ),
            )
    }
}

impl TextInput {
    /// The clear button of a [`Self::clearable`] field.
    fn render_clear_button(&self, label: SharedString, cx: &mut Context<Self>) -> AnyElement {
        let id = ElementId::NamedInteger("text-input-clear".into(), cx.entity_id().as_u64());
        div()
            .flex_none()
            .debug_selector(|| "text-input-clear".into())
            // The press would otherwise place the cursor in the field.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                IconButton::new(id, "x", label)
                    .size(ButtonSize::Small)
                    .on_click(cx.listener(|input, _, window, cx| {
                        input.clear(cx);
                        input.focus(window, cx);
                    })),
            )
            .into_any_element()
    }
}

/// Paints the text, selection and cursor of a [`TextInput`] and registers
/// it as the window's IME target while focused.
struct TextElement {
    input: Entity<TextInput>,
}

struct PrepaintState {
    layout: Rc<InputLayout>,
    selections: Vec<PaintQuad>,
    cursor: Option<PaintQuad>,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// What to shape for an input: its display text, or the placeholder.
fn shaping_input(
    input: &TextInput,
    colors: &UiColors,
    window: &Window,
) -> (SharedString, Vec<TextRun>, bool) {
    let style = window.text_style();
    let display = input.display_text();
    if display.is_empty() {
        let placeholder = input.placeholder.clone();
        let run = TextRun {
            len: placeholder.len(),
            font: style.font(),
            color: rgb(colors.muted).into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        return (placeholder, vec![run], true);
    }

    let color: Hsla = if input.disabled {
        rgb(colors.muted).into()
    } else {
        rgb(colors.foreground).into()
    };
    let run = TextRun {
        len: display.len(),
        font: style.font(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let runs = match input.marked_range.as_ref() {
        Some(marked) => {
            let start = input.content_to_display(marked.start);
            let end = input.content_to_display(marked.end).max(start);
            vec![
                TextRun {
                    len: start,
                    ..run.clone()
                },
                TextRun {
                    len: end - start,
                    underline: Some(UnderlineStyle {
                        color: Some(color),
                        thickness: px(1.),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: display.len() - end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect()
        }
        None => vec![run],
    };
    (display.into(), runs, false)
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let line_height = window.line_height();
        let mut style = Style::default();
        style.size.width = relative(1.).into();

        let input = self.input.read(cx);
        if !input.multiline {
            style.size.height = line_height.into();
            return (window.request_layout(style, [], cx), ());
        }

        // Multi-line inputs grow with their wrapped text between their
        // minimum and maximum rows.
        let colors = palette(cx);
        let (text, runs, _) = shaping_input(input, &colors, window);
        let font_size = window.text_style().font_size.to_pixels(window.rem_size());
        let (min_rows, max_rows) = (input.min_rows, input.max_rows);
        let layout_id =
            window.request_measured_layout(style, move |known, available, window, _| {
                let width = known.width.or(match available.width {
                    gpui::AvailableSpace::Definite(width) => Some(width),
                    _ => None,
                });
                let rows = window
                    .text_system()
                    .shape_text(text.clone(), font_size, &runs, width, None)
                    .map(|lines| {
                        lines
                            .iter()
                            .map(|line| line.wrap_boundaries().len() + 1)
                            .sum::<usize>()
                    })
                    .unwrap_or(1);
                size(
                    width.unwrap_or_default(),
                    line_height * rows.clamp(min_rows, max_rows) as f32,
                )
            });
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let colors = palette(cx);
        let line_height = window.line_height();
        let font_size = window.text_style().font_size.to_pixels(window.rem_size());

        let input = self.input.read(cx);
        let (text, runs, is_placeholder) = shaping_input(input, &colors, window);
        let multiline = input.multiline;
        let selected = input.content_to_display(input.selected_range.start)
            ..input.content_to_display(input.selected_range.end);
        let cursor = input.content_to_display(input.cursor_offset());
        let mut scroll = input.scroll_offset;
        let autoscroll = input.autoscroll;

        let wrap_width = multiline.then_some(bounds.size.width);
        let lines: Vec<WrappedLine> = window
            .text_system()
            .shape_text(text.clone(), font_size, &runs, wrap_width, None)
            .map(|lines| lines.into_iter().collect())
            .unwrap_or_default();
        let mut line_starts = Vec::with_capacity(lines.len());
        let mut start = 0;
        for line in &lines {
            line_starts.push(start);
            start += line.len() + 1;
        }
        let mut layout = InputLayout {
            lines,
            line_starts,
            line_height,
            text_origin: bounds.origin,
            bounds,
            is_placeholder,
        };

        // Keep the cursor visible and the scroll offset in range.
        let cursor_position = layout.position_for_offset(cursor);
        if multiline {
            let max_scroll = (layout.content_height() - bounds.size.height).max(px(0.));
            if autoscroll {
                if cursor_position.y < scroll.y {
                    scroll.y = cursor_position.y;
                } else if cursor_position.y + line_height > scroll.y + bounds.size.height {
                    scroll.y = cursor_position.y + line_height - bounds.size.height;
                }
            }
            scroll = point(px(0.), scroll.y.clamp(px(0.), max_scroll));
        } else {
            let max_scroll =
                (layout.content_width() + CURSOR_WIDTH - bounds.size.width).max(px(0.));
            if autoscroll {
                if cursor_position.x < scroll.x {
                    scroll.x = cursor_position.x;
                } else if cursor_position.x + CURSOR_WIDTH > scroll.x + bounds.size.width {
                    scroll.x = cursor_position.x + CURSOR_WIDTH - bounds.size.width;
                }
            }
            scroll = point(scroll.x.clamp(px(0.), max_scroll), px(0.));
        }
        layout.text_origin = bounds.origin - scroll;
        let origin = layout.text_origin;

        let selection_color = with_alpha(colors.accent, 0.35);
        let mut selections = Vec::new();
        let cursor_quad = if selected.is_empty() || is_placeholder {
            Some(fill(
                Bounds::new(origin + cursor_position, size(CURSOR_WIDTH, line_height)),
                rgb(colors.accent),
            ))
        } else {
            let start = layout.position_for_offset(selected.start);
            let end = layout.position_for_offset(selected.end);
            let right = bounds.size.width.max(layout.content_width());
            if start.y == end.y {
                selections.push(fill(
                    Bounds::from_corners(
                        origin + start,
                        origin + point(end.x, end.y + line_height),
                    ),
                    selection_color,
                ));
            } else {
                selections.push(fill(
                    Bounds::from_corners(
                        origin + start,
                        origin + point(right, start.y + line_height),
                    ),
                    selection_color,
                ));
                if end.y > start.y + line_height {
                    selections.push(fill(
                        Bounds::from_corners(
                            origin + point(px(0.), start.y + line_height),
                            origin + point(right, end.y),
                        ),
                        selection_color,
                    ));
                }
                selections.push(fill(
                    Bounds::from_corners(
                        origin + point(px(0.), end.y),
                        origin + point(end.x, end.y + line_height),
                    ),
                    selection_color,
                ));
            }
            None
        };

        let layout = Rc::new(layout);
        self.input.update(cx, |input, _| {
            input.layout = Some(layout.clone());
            input.scroll_offset = scroll;
            input.autoscroll = false;
        });

        PrepaintState {
            layout,
            selections,
            cursor: cursor_quad,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let input = self.input.read(cx);
        let focus_handle = input.focus_handle.clone();
        let disabled = input.disabled;
        let focused = !disabled && focus_handle.is_focused(window);
        if focused {
            window.handle_input(
                &focus_handle,
                ElementInputHandler::new(bounds, self.input.clone()),
                cx,
            );
        }

        let layout = prepaint.layout.clone();
        let selections = std::mem::take(&mut prepaint.selections);
        let cursor = prepaint.cursor.take();
        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            for selection in selections {
                window.paint_quad(selection);
            }
            let mut top = layout.text_origin.y;
            for line in &layout.lines {
                let _ = line.paint(
                    point(layout.text_origin.x, top),
                    layout.line_height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
                top += line.size(layout.line_height).height;
            }
            if focused && let Some(cursor) = cursor {
                window.paint_quad(cursor);
            }
        });
    }
}
