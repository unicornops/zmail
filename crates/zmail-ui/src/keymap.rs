//! Loads key bindings from a keymap file.
//!
//! The format is the subset of Zed's keymap format Zmail needs: a JSON
//! array of sections, each with an optional `context` predicate and a
//! `bindings` object mapping keystrokes to action names:
//!
//! ```json
//! [
//!   { "context": "MessageList", "bindings": { "down": "zmail::SelectNext" } }
//! ]
//! ```
//!
//! Keystrokes use GPUI syntax. `secondary-` means Cmd on macOS and Ctrl on
//! Linux and Windows, so one keymap serves every platform. An action can
//! also be written as `["zmail::Name", { ...arguments }]` for actions that
//! take arguments.
//!
//! Order matters in one way: the native macOS menu bar shows an action's
//! *first* binding as the menu item's key equivalent, and AppKit fires that
//! equivalent whenever GPUI leaves the keystroke unhandled. So an action
//! that is in a menu lists a chorded binding first (Delete lists
//! `secondary-backspace` before plain `delete`), or a plain key would fire
//! it from panes that never bound it. `menus::tests` checks this.
//!
//! The default keymap ships inside the binary, plus a small macOS keymap for
//! the system conventions that only exist there (Hide, Minimize).
//!
//! The user picks a [`KeymapName`] (saved in the UI preferences): the
//! standard keymap, or the Gmail keymap, which layers single-key shortcuts
//! on top of it. [`bind_keymap`] binds everything the app binds for one of
//! them, and [`use_keymap`] switches at runtime without a restart. The
//! Gmail keys are mostly plain letters, so they only apply in the message
//! list and reader (and a few window-wide) and never while a text field
//! has focus. A user keymap file is still to come.

use std::rc::Rc;

use anyhow::{Context as _, Result, anyhow, bail};
use gpui::{App, Global, KeyBinding, KeyBindingContextPredicate};
use serde_json::Value;
use zmail_core::preferences::KeymapName;
use zmail_store::LocalStore;

use crate::components;

/// The keymap bundled with the app.
pub(crate) const DEFAULT_KEYMAP: &str = include_str!("../keymaps/default.json");

/// macOS-only bindings, loaded after the default keymap on macOS.
pub(crate) const MACOS_KEYMAP: &str = include_str!("../keymaps/macos.json");

/// The Gmail keymap's single-key shortcuts, loaded on top of the platform
/// keymaps. Only New Message's c and Go to Inbox's g i are window-wide; z
/// for Undo is bound in the panes so the Edit menu keeps showing the
/// standard Undo chord (see `menus::menu_binding`). Every section excludes text fields (`!TextInput`), so the keys
/// type normally there, and the open overlays (`!Modal`, `!Menu`,
/// `!Select`) and compose (`!Compose`), whose controls would otherwise
/// pass the keys on to the conversation behind them.
pub(crate) const GMAIL_KEYMAP: &str = include_str!("../keymaps/gmail.json");

/// The bundled keymaps this platform loads, in order.
pub(crate) fn platform_keymaps() -> &'static [&'static str] {
    if cfg!(target_os = "macos") {
        &[DEFAULT_KEYMAP, MACOS_KEYMAP]
    } else {
        &[DEFAULT_KEYMAP]
    }
}

/// Parses a keymap file into bindings. Every action must be registered and
/// every keystroke and context must parse; the first problem is an error,
/// so a broken keymap fails loudly in tests rather than dropping keys.
pub(crate) fn parse_keymap(source: &str, cx: &App) -> Result<Vec<KeyBinding>> {
    let sections: Value = serde_json::from_str(source).context("the keymap is not valid JSON")?;
    let sections = sections
        .as_array()
        .context("the keymap must be a JSON array of sections")?;

    let mut bindings = Vec::new();
    for (index, section) in sections.iter().enumerate() {
        let section = section
            .as_object()
            .with_context(|| format!("keymap section {index} is not an object"))?;
        let context = match section.get("context") {
            None | Some(Value::Null) => None,
            Some(Value::String(context)) => Some(Rc::new(
                KeyBindingContextPredicate::parse(context)
                    .with_context(|| format!("keymap section {index}: bad context {context:?}"))?,
            )),
            Some(other) => bail!("keymap section {index}: context must be a string, not {other}"),
        };
        let entries = section
            .get("bindings")
            .and_then(Value::as_object)
            .with_context(|| format!("keymap section {index} has no bindings object"))?;

        for (keystrokes, target) in entries {
            let (name, arguments) = match target {
                Value::String(name) => (name.as_str(), None),
                Value::Array(parts) => match parts.as_slice() {
                    [Value::String(name), arguments] => (name.as_str(), Some(arguments.clone())),
                    _ => bail!(
                        "keymap section {index}, {keystrokes:?}: expected [\"action\", {{arguments}}]"
                    ),
                },
                other => bail!("keymap section {index}, {keystrokes:?}: bad action {other}"),
            };
            let action = cx.build_action(name, arguments).map_err(|error| {
                anyhow!("keymap section {index}, {keystrokes:?}: action {name}: {error}")
            })?;
            let binding = KeyBinding::load(
                keystrokes,
                action,
                context.clone(),
                false,
                None,
                cx.keyboard_mapper().as_ref(),
            )
            .map_err(|error| anyhow!("keymap section {index}, {keystrokes:?}: {error}"))?;
            bindings.push(binding);
        }
    }
    Ok(bindings)
}

/// The bundled keymaps `keymap` loads on this platform, in order: later
/// files win over earlier ones where both bind a key in the same context.
pub(crate) fn keymaps_for(keymap: KeymapName) -> Vec<&'static str> {
    let mut sources = platform_keymaps().to_vec();
    if keymap == KeymapName::Gmail {
        sources.push(GMAIL_KEYMAP);
    }
    sources
}

/// The keymap the app has bound, so views can say which one is active.
struct ActiveKeymap(KeymapName);

impl Global for ActiveKeymap {}

/// The keymap [`bind_keymap`] last bound (the standard one before that).
// Read by Settings → Keyboard, which is being built alongside this.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn active_keymap(cx: &App) -> KeymapName {
    cx.try_global::<ActiveKeymap>()
        .map_or_else(KeymapName::default, |active| active.0)
}

/// Replaces every key binding with the ones for `keymap`: the components',
/// the text input's and the bundled keymap files. Call it at startup before
/// [`crate::menus::init`], and through [`use_keymap`] afterwards.
///
/// A parse failure is a bug in a bundled file (the tests load them), so it
/// is logged rather than stopping the app.
pub(crate) fn bind_keymap(keymap: KeymapName, cx: &mut App) {
    cx.clear_key_bindings();
    cx.bind_keys(components::actions::key_bindings());
    cx.bind_keys(components::text_input::key_bindings());
    for source in keymaps_for(keymap) {
        match parse_keymap(source, cx) {
            Ok(bindings) => cx.bind_keys(bindings),
            Err(error) => eprintln!("zmail: a bundled keymap did not load: {error:#}"),
        }
    }
    cx.set_global(ActiveKeymap(keymap));
}

/// Switches to `keymap` while the app runs: saves the choice, rebinds the
/// keys and refreshes the macOS menu bar's key equivalents. The keys
/// change even if saving fails; the error says the choice will not survive
/// a restart.
// Called by Settings → Keyboard, which is being built alongside this.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn use_keymap(store: &LocalStore, keymap: KeymapName, cx: &mut App) -> Result<()> {
    if active_keymap(cx) != keymap {
        bind_keymap(keymap, cx);
        crate::menus::refresh_menu_bar(cx);
    }
    store
        .update_ui_preferences(|preferences| preferences.keymap = keymap)
        .context("could not save the keyboard shortcut choice")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use gpui::TestAppContext;
    use zmail_core::preferences::KeymapName;
    use zmail_store::LocalStore;

    use super::{
        DEFAULT_KEYMAP, GMAIL_KEYMAP, MACOS_KEYMAP, active_keymap, bind_keymap, parse_keymap,
        use_keymap,
    };
    use crate::actions::{ACTIONS_WITHOUT_KEYS, NewMessage};
    use crate::components::text_input;

    #[gpui::test]
    fn binding_a_keymap_replaces_every_binding(cx: &mut TestAppContext) {
        let count =
            |cx: &mut TestAppContext| cx.update(|cx| cx.key_bindings().borrow().bindings().len());
        cx.update(|cx| bind_keymap(KeymapName::Default, cx));
        let standard = count(cx);
        assert!(standard > 0);
        cx.update(|cx| {
            assert_eq!(active_keymap(cx), KeymapName::Default);
            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            assert!(
                keymap.bindings_for_action(&NewMessage).next().is_some(),
                "the keymap files are bound"
            );
            assert!(
                keymap
                    .bindings_for_action(&text_input::Paste)
                    .next()
                    .is_some(),
                "the text input's own keys are bound"
            );
        });
        // Binding again replaces rather than adds.
        cx.update(|cx| bind_keymap(KeymapName::Default, cx));
        assert_eq!(count(cx), standard);
        cx.update(|cx| bind_keymap(KeymapName::Gmail, cx));
        cx.update(|cx| assert_eq!(active_keymap(cx), KeymapName::Gmail));
        assert!(
            count(cx) > standard,
            "the Gmail layer adds to the standard keys"
        );
        cx.update(|cx| bind_keymap(KeymapName::Default, cx));
        assert_eq!(count(cx), standard, "switching back drops the Gmail layer");
    }

    #[gpui::test]
    fn switching_keymaps_saves_the_choice(cx: &mut TestAppContext) {
        let store = LocalStore::open_ephemeral().expect("the store opens");
        cx.update(|cx| {
            assert_eq!(active_keymap(cx), KeymapName::Default, "before any binding");
            bind_keymap(KeymapName::Default, cx);
            use_keymap(&store, KeymapName::Gmail, cx).expect("the choice saves");
            assert_eq!(active_keymap(cx), KeymapName::Gmail);
        });
        let saved = store.ui_preferences().expect("the preferences load");
        assert_eq!(saved.keymap, KeymapName::Gmail);
        cx.update(|cx| {
            use_keymap(&store, KeymapName::Default, cx).expect("the choice saves");
            assert_eq!(active_keymap(cx), KeymapName::Default);
        });
        let saved = store.ui_preferences().expect("the preferences load");
        assert_eq!(saved.keymap, KeymapName::Default);
    }

    #[gpui::test]
    fn the_bundled_keymaps_load(cx: &mut TestAppContext) {
        for source in [DEFAULT_KEYMAP, MACOS_KEYMAP, GMAIL_KEYMAP] {
            let bindings = cx
                .update(|cx| parse_keymap(source, cx))
                .expect("the bundled keymap parses");
            assert!(!bindings.is_empty());
        }
    }

    #[gpui::test]
    fn every_action_has_a_default_binding(cx: &mut TestAppContext) {
        // Checked against both keymaps on every platform, so a Linux test
        // run also covers the macOS-only bindings.
        let (bound, registered) = cx.update(|cx| {
            let mut bindings = parse_keymap(DEFAULT_KEYMAP, cx).expect("the keymap parses");
            bindings.extend(parse_keymap(MACOS_KEYMAP, cx).expect("the macOS keymap parses"));
            let bound = bindings
                .iter()
                .map(|binding| binding.action().name())
                .collect::<BTreeSet<_>>();
            let registered = cx
                .all_action_names()
                .iter()
                .copied()
                .filter(|name| name.starts_with("zmail::"))
                .collect::<BTreeSet<_>>();
            (bound, registered)
        });
        let unbound = registered
            .iter()
            .filter(|name| {
                !bound.contains(*name)
                    && !ACTIONS_WITHOUT_KEYS
                        .iter()
                        .any(|(unbound, _)| unbound == *name)
            })
            .collect::<Vec<_>>();
        assert!(unbound.is_empty(), "actions without a key: {unbound:?}");
    }

    #[gpui::test]
    fn plain_keys_are_only_bound_inside_panes(cx: &mut TestAppContext) {
        // A key without Cmd/Ctrl/Alt bound at the Workspace level would
        // fire while typing in the search field. Only navigation keys that
        // text fields do not type may live there.
        const WORKSPACE_PLAIN_KEYS: &[&str] = &["tab", "f5", "f6", "escape"];
        let bindings = cx
            .update(|cx| parse_keymap(DEFAULT_KEYMAP, cx))
            .expect("the keymap parses");
        for binding in &bindings {
            let context = binding
                .predicate()
                .map(|predicate| predicate.to_string())
                .unwrap_or_default();
            if context != "Workspace" {
                continue;
            }
            for keystroke in binding.keystrokes() {
                let modifiers = keystroke.modifiers();
                let chord = modifiers.platform || modifiers.control || modifiers.alt;
                assert!(
                    chord || WORKSPACE_PLAIN_KEYS.contains(&keystroke.key()),
                    "{keystroke:?} is a plain key bound at the Workspace level"
                );
            }
        }
    }

    #[gpui::test]
    fn gmail_keys_never_apply_in_text_fields_or_overlays(cx: &mut TestAppContext) {
        // Plain letters would otherwise fire while typing, or act on the
        // conversation behind an open menu, dialog or compose form.
        let bindings = cx
            .update(|cx| parse_keymap(GMAIL_KEYMAP, cx))
            .expect("the Gmail keymap parses");
        for binding in &bindings {
            let context = binding
                .predicate()
                .map(|predicate| predicate.to_string())
                .unwrap_or_default();
            for excluded in ["!TextInput", "!Modal", "!Menu", "!Select", "!Compose"] {
                assert!(
                    context.contains(excluded),
                    "{:?} in {context:?} lacks {excluded}",
                    binding.keystrokes()
                );
            }
        }
        let sequences = bindings
            .iter()
            .filter(|binding| binding.keystrokes().len() > 1)
            .count();
        assert_eq!(sequences, 1, "g i is the one two-key sequence");
    }

    #[gpui::test]
    fn bad_keymaps_are_reported(cx: &mut TestAppContext) {
        cx.update(|cx| {
            for (source, expected) in [
                ("{}", "JSON array"),
                (r#"[{"bindings": {"x": "zmail::Nope"}}]"#, "zmail::Nope"),
                (r#"[{"context": "A &&", "bindings": {}}]"#, "bad context"),
                (r#"[{"context": "Workspace"}]"#, "no bindings"),
                (r#"[{"bindings": {"x": 3}}]"#, "bad action"),
            ] {
                let error = parse_keymap(source, cx).expect_err(source);
                assert!(
                    format!("{error:#}").contains(expected),
                    "{source}: {error:#}"
                );
            }
        });
    }
}
