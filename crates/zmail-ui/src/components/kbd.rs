//! `Kbd`: a keyboard shortcut drawn as a key cap, with platform names.
//!
//! Shortcuts are written the way GPUI key bindings are (`"secondary-k"`,
//! `"ctrl-shift-tab"`, `"g i"` for a two-key sequence). `secondary` is the
//! platform's primary modifier: Cmd on macOS, Ctrl elsewhere. macOS shows
//! symbols in Apple's modifier order (`⌃⌥⇧⌘K`); Windows and Linux spell
//! the names out and join them with `+` (`Ctrl+Shift+K`).
//!
//! A key cap has no accessibility node: the control or list item it
//! belongs to carries the shortcut, spelled out by [`spoken_keystrokes`].

use gpui::{
    App, FontWeight, IntoElement, RenderOnce, SharedString, Window, div, prelude::*, px, rgb,
};

use crate::styles::{RADIUS_SM, TYPE_CAPTION, TypeStyleExt};

use crate::styles::Platform;

use super::theme::ComponentTheme;

/// A keyboard shortcut rendered as a key cap.
#[derive(IntoElement)]
pub(crate) struct Kbd {
    keystrokes: SharedString,
    platform: Option<Platform>,
}

impl Kbd {
    /// A key cap for `keystrokes` (GPUI binding syntax).
    pub(crate) fn new(keystrokes: impl Into<SharedString>) -> Self {
        Self {
            keystrokes: keystrokes.into(),
            platform: None,
        }
    }

    /// Label the keys for `platform` instead of the theme's platform.
    pub(crate) fn platform(mut self, platform: Platform) -> Self {
        self.platform = Some(platform);
        self
    }
}

impl RenderOnce for Kbd {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = ComponentTheme::get(cx);
        let platform = self.platform.unwrap_or(theme.platform.platform);
        let label = keystrokes_label(&self.keystrokes, platform);
        div()
            .flex_none()
            .px(px(5.0))
            .min_w(px(18.0))
            .rounded(px(RADIUS_SM))
            .border_1()
            .border_color(rgb(theme.colors.border))
            .bg(rgb(theme.colors.surface))
            .type_style(TYPE_CAPTION)
            .font_weight(FontWeight::MEDIUM)
            .text_color(rgb(theme.colors.muted))
            .text_center()
            .whitespace_nowrap()
            .child(label)
    }
}

/// The modifiers of one keystroke, normalised across spellings.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Modifiers {
    control: bool,
    alt: bool,
    shift: bool,
    platform: bool,
    function: bool,
}

/// Label a whole key sequence (`"g i"`) for `platform`.
pub(crate) fn keystrokes_label(keystrokes: &str, platform: Platform) -> String {
    keystrokes
        .split_whitespace()
        .map(|keystroke| keystroke_label(keystroke, platform))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Spell a whole key sequence out in words for a screen reader:
/// "Shift+Command+K" on macOS, "Ctrl+Shift+K" elsewhere, and
/// "G, then I" for a two-key sequence. Symbols such as ⌘ and ↩ read
/// inconsistently across screen readers, so none are used.
pub(crate) fn spoken_keystrokes(keystrokes: &str, platform: Platform) -> String {
    keystrokes
        .split_whitespace()
        .map(|keystroke| spoken_keystroke(keystroke, platform))
        .collect::<Vec<_>>()
        .join(", then ")
}

/// Spell a single keystroke out in words.
fn spoken_keystroke(keystroke: &str, platform: Platform) -> String {
    let (modifiers, key) = parse_keystroke(keystroke, platform);
    let mac = platform == Platform::MacOs;
    let platform_key = match platform {
        Platform::MacOs => "Command",
        Platform::Windows => "Windows",
        Platform::Linux => "Super",
    };
    let mut parts: Vec<String> = Vec::new();
    for (on, name) in [
        (modifiers.control, if mac { "Control" } else { "Ctrl" }),
        (modifiers.alt, if mac { "Option" } else { "Alt" }),
        (modifiers.shift, "Shift"),
        (modifiers.platform, platform_key),
        (modifiers.function, "Fn"),
    ] {
        if on {
            parts.push(name.to_owned());
        }
    }
    parts.push(spoken_key_name(key, platform));
    parts.join("+")
}

/// The spoken name of a key.
fn spoken_key_name(key: &str, platform: Platform) -> String {
    let mac = platform == Platform::MacOs;
    let named = match key.to_ascii_lowercase().as_str() {
        "enter" | "return" => Some(if mac { "Return" } else { "Enter" }),
        // The Mac keyboard calls Backspace "Delete".
        "backspace" => Some(if mac { "Delete" } else { "Backspace" }),
        "delete" => Some(if mac { "Forward Delete" } else { "Delete" }),
        "escape" | "esc" => Some("Escape"),
        "tab" => Some("Tab"),
        "space" => Some("Space"),
        "up" => Some("Up Arrow"),
        "down" => Some("Down Arrow"),
        "left" => Some("Left Arrow"),
        "right" => Some("Right Arrow"),
        "pageup" => Some("Page Up"),
        "pagedown" => Some("Page Down"),
        "home" => Some("Home"),
        "end" => Some("End"),
        "insert" => Some("Insert"),
        "-" => Some("Minus"),
        "=" => Some("Equals"),
        "," => Some("Comma"),
        "." => Some("Period"),
        "/" => Some("Slash"),
        "\\" => Some("Backslash"),
        "?" => Some("Question Mark"),
        "#" => Some("Number Sign"),
        "[" => Some("Left Bracket"),
        "]" => Some("Right Bracket"),
        _ => None,
    };
    match named {
        Some(named) => named.to_owned(),
        // Letters, digits and function keys read fine as drawn.
        None => key_name(key, Platform::Linux),
    }
}

/// Label a single keystroke (`"secondary-shift-k"`) for `platform`.
pub(crate) fn keystroke_label(keystroke: &str, platform: Platform) -> String {
    let (modifiers, key) = parse_keystroke(keystroke, platform);
    let key = key_name(key, platform);

    if platform == Platform::MacOs {
        let mut label = String::new();
        // Apple's order: Control, Option, Shift, Command.
        for (on, symbol) in [
            (modifiers.function, "fn"),
            (modifiers.control, "⌃"),
            (modifiers.alt, "⌥"),
            (modifiers.shift, "⇧"),
            (modifiers.platform, "⌘"),
        ] {
            if on {
                label.push_str(symbol);
            }
        }
        label.push_str(&key);
        label
    } else {
        let platform_key = if platform == Platform::Windows {
            "Win"
        } else {
            "Super"
        };
        let mut parts: Vec<&str> = Vec::new();
        for (on, name) in [
            (modifiers.control, "Ctrl"),
            (modifiers.alt, "Alt"),
            (modifiers.shift, "Shift"),
            (modifiers.platform, platform_key),
            (modifiers.function, "Fn"),
        ] {
            if on {
                parts.push(name);
            }
        }
        parts.push(&key);
        parts.join("+")
    }
}

/// Split `keystroke` into modifiers and the key. A trailing `-` is the
/// minus key itself (`"ctrl--"`).
fn parse_keystroke(keystroke: &str, platform: Platform) -> (Modifiers, &str) {
    let mut modifiers = Modifiers::default();
    let mut rest = keystroke;
    while let Some((head, tail)) = rest.split_once('-') {
        if tail.is_empty() {
            // `head-` with nothing after: the key is `-`.
            if apply_modifier(&mut modifiers, head, platform) {
                return (modifiers, "-");
            }
            break;
        }
        if !apply_modifier(&mut modifiers, head, platform) {
            break;
        }
        rest = tail;
    }
    (modifiers, rest)
}

/// Record `name` as a modifier, returning whether it was one.
fn apply_modifier(modifiers: &mut Modifiers, name: &str, platform: Platform) -> bool {
    match name.to_ascii_lowercase().as_str() {
        "ctrl" | "control" => modifiers.control = true,
        "alt" | "option" | "opt" => modifiers.alt = true,
        "shift" => modifiers.shift = true,
        "cmd" | "super" | "win" | "platform" | "meta" => modifiers.platform = true,
        "fn" | "function" => modifiers.function = true,
        "secondary" => {
            if platform == Platform::MacOs {
                modifiers.platform = true;
            } else {
                modifiers.control = true;
            }
        }
        _ => return false,
    }
    true
}

/// The display name of a key.
fn key_name(key: &str, platform: Platform) -> String {
    let mac = platform == Platform::MacOs;
    let named = match key.to_ascii_lowercase().as_str() {
        "enter" | "return" => Some(if mac { "↩" } else { "Enter" }),
        "backspace" => Some(if mac { "⌫" } else { "Backspace" }),
        "delete" => Some(if mac { "⌦" } else { "Del" }),
        "escape" | "esc" => Some(if mac { "⎋" } else { "Esc" }),
        "tab" => Some(if mac { "⇥" } else { "Tab" }),
        "space" => Some("Space"),
        "up" => Some("↑"),
        "down" => Some("↓"),
        "left" => Some("←"),
        "right" => Some("→"),
        "pageup" => Some(if mac { "⇞" } else { "PgUp" }),
        "pagedown" => Some(if mac { "⇟" } else { "PgDn" }),
        "home" => Some(if mac { "↖" } else { "Home" }),
        "end" => Some(if mac { "↘" } else { "End" }),
        "insert" => Some("Ins"),
        _ => None,
    };
    if let Some(named) = named {
        return named.to_owned();
    }
    // Function keys and single characters are shown in capitals.
    let lower = key.to_ascii_lowercase();
    if lower.len() > 1
        && let Some(number) = lower.strip_prefix('f')
        && number.chars().all(|ch| ch.is_ascii_digit())
    {
        return format!("F{number}");
    }
    if key.chars().count() == 1 {
        return key.to_uppercase();
    }
    // Anything else: capitalise the first letter.
    let mut chars = key.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    use Platform::{Linux, MacOs as Mac, Windows};

    #[test]
    fn secondary_is_command_on_mac_and_control_elsewhere() {
        assert_eq!(keystroke_label("secondary-k", Mac), "⌘K");
        assert_eq!(keystroke_label("secondary-k", Windows), "Ctrl+K");
        assert_eq!(keystroke_label("secondary-k", Linux), "Ctrl+K");
    }

    #[test]
    fn modifiers_follow_each_platforms_order() {
        // Apple order is Control, Option, Shift, Command regardless of how
        // the binding was written.
        assert_eq!(keystroke_label("cmd-shift-alt-ctrl-z", Mac), "⌃⌥⇧⌘Z");
        assert_eq!(keystroke_label("shift-ctrl-z", Linux), "Ctrl+Shift+Z");
        assert_eq!(
            keystroke_label("alt-shift-ctrl-z", Windows),
            "Ctrl+Alt+Shift+Z"
        );
    }

    #[test]
    fn platform_key_is_named_per_os() {
        assert_eq!(keystroke_label("super-l", Linux), "Super+L");
        assert_eq!(keystroke_label("super-l", Windows), "Win+L");
        assert_eq!(keystroke_label("cmd-l", Mac), "⌘L");
    }

    #[test]
    fn named_keys_use_symbols_on_mac_and_words_elsewhere() {
        assert_eq!(keystroke_label("secondary-enter", Mac), "⌘↩");
        assert_eq!(keystroke_label("secondary-enter", Linux), "Ctrl+Enter");
        assert_eq!(keystroke_label("escape", Mac), "⎋");
        assert_eq!(keystroke_label("escape", Windows), "Esc");
        assert_eq!(keystroke_label("backspace", Mac), "⌫");
        assert_eq!(keystroke_label("delete", Linux), "Del");
        assert_eq!(keystroke_label("shift-tab", Linux), "Shift+Tab");
        assert_eq!(keystroke_label("up", Linux), "↑");
        assert_eq!(keystroke_label("f5", Windows), "F5");
        assert_eq!(keystroke_label("pagedown", Linux), "PgDn");
    }

    #[test]
    fn punctuation_and_minus_keys() {
        assert_eq!(keystroke_label("secondary-,", Mac), "⌘,");
        assert_eq!(keystroke_label("ctrl--", Linux), "Ctrl+-");
        assert_eq!(keystroke_label("-", Linux), "-");
        assert_eq!(keystroke_label("shift-/", Linux), "Shift+/");
    }

    #[test]
    fn sequences_label_each_keystroke() {
        assert_eq!(keystrokes_label("g i", Mac), "G I");
        assert_eq!(
            keystrokes_label("secondary-k secondary-s", Linux),
            "Ctrl+K Ctrl+S"
        );
        assert_eq!(keystrokes_label("secondary-k secondary-s", Mac), "⌘K ⌘S");
    }

    #[test]
    fn spoken_shortcuts_spell_out_modifiers_and_keys() {
        // Apple order on macOS, as its menus write it.
        assert_eq!(
            spoken_keystrokes("secondary-shift-k", Mac),
            "Shift+Command+K"
        );
        assert_eq!(
            spoken_keystrokes("secondary-shift-k", Linux),
            "Ctrl+Shift+K"
        );
        assert_eq!(spoken_keystrokes("alt-up", Mac), "Option+Up Arrow");
        assert_eq!(spoken_keystrokes("super-l", Windows), "Windows+L");
        assert_eq!(spoken_keystrokes("secondary-enter", Mac), "Command+Return");
        assert_eq!(spoken_keystrokes("backspace", Mac), "Delete");
        assert_eq!(spoken_keystrokes("shift-/", Linux), "Shift+Slash");
        assert_eq!(spoken_keystrokes("shift-3", Linux), "Shift+3");
        assert_eq!(spoken_keystrokes("g i", Linux), "G, then I");
        assert_eq!(spoken_keystrokes("f5", Windows), "F5");
    }

    /// The rendered cap uses the platform from the active theme.
    #[gpui::test]
    fn kbd_renders_the_theme_platform(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let theme = ComponentTheme::fallback(zmail_core::ThemeAppearance::Dark);
            ComponentTheme::set(theme.with_platform(Mac), cx);
            assert_eq!(ComponentTheme::get(cx).platform.platform, Mac);
            ComponentTheme::set(theme.with_platform(Windows), cx);
            assert_eq!(ComponentTheme::get(cx).platform.platform, Windows);
        });
        let window = cx.add_window(|_, _| KbdView);
        window
            .update(cx, |_, window, cx| {
                // Drawing must not panic for any platform.
                window.refresh();
                cx.notify();
            })
            .ok();
        cx.run_until_parked();
    }

    struct KbdView;

    impl gpui::Render for KbdView {
        fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
            div()
                .child(Kbd::new("secondary-shift-k"))
                .child(Kbd::new("escape").platform(Mac))
        }
    }
}
