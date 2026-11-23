//! The documentation's screenshots of the terminal UI
//! (`docs/images/tui-*.png`, docs/site.md): screens of the snapshot tests'
//! synthetic mail, drawn on the test terminal and rendered to PNG by
//! `screenshot.rs`, with Zmail's real version in the title bar.
//!
//! `scripts/capture-tui-screenshots.sh OUT_DIR` runs the ignored
//! [`capture_documentation_screenshots`] with the pinned fonts it
//! downloads; its `STATES` list must name exactly [`SCREENSHOTS`].

use std::path::{Path, PathBuf};

use crossterm::event::KeyCode;

use super::{
    app, draw, key, keys, open_work_inbox,
    screenshot::{Palette, Renderer},
};
use crate::app::{App, Screen};

/// One screenshot: its name (the image is `<name>.png`), the terminal's
/// size, and how to reach the screen from the Main Menu.
struct Shot {
    name: &'static str,
    width: u16,
    height: u16,
    open: fn(&mut App, u16, u16),
}

const SCREENSHOTS: &[Shot] = &[
    // The Work inbox with the preview pane (120 columns or wider): the
    // conversation under the cursor beside the index.
    Shot {
        name: "tui-message-index",
        width: 120,
        height: 30,
        open: |app, width, height| {
            open_work_inbox(app, width, height);
            assert_eq!(app.screen, Screen::Index);
        },
    },
    // The HTML newsletter in Message Text, with its links listed.
    Shot {
        name: "tui-message-text",
        width: 100,
        height: 30,
        open: |app, width, height| {
            open_work_inbox(app, width, height);
            key(app, KeyCode::Down);
            key(app, KeyCode::Enter);
            assert_eq!(app.screen, Screen::Message);
        },
    },
    // A reply being written, quoting the message read.
    Shot {
        name: "tui-compose-reply",
        width: 100,
        height: 30,
        open: |app, width, height| {
            open_work_inbox(app, width, height);
            key(app, KeyCode::Enter);
            draw(app, width, height);
            keys(app, "r");
            assert_eq!(app.screen, Screen::Compose);
            keys(app, "Thanks, Grace. Triage is next on the list.");
        },
    },
];

/// The font size, in pixels per em: twice a usual terminal's, so the
/// images stay sharp on high-density screens.
const FONT_SIZE: f32 = 30.0;

/// The fonts `capture-tui-screenshots.sh` downloads into the folder it
/// names, in the order glyphs are looked for: monospaced Latin first, then
/// Japanese, then emoji.
const FONTS: &[&str] = &[
    "JetBrainsMono[wght].ttf",
    "NotoSansJP[wght].ttf",
    "NotoEmoji[wght].ttf",
];

/// Zmail's version, from zmail-app's manifest (the version a release
/// builds; the snapshot tests keep their fixed `0.0.0-test`).
fn zmail_version() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../zmail-app/Cargo.toml");
    let manifest = std::fs::read_to_string(&path).expect("zmail-app's manifest");
    manifest
        .lines()
        .take_while(|line| !line.starts_with('[') || line.trim() == "[package]")
        .find_map(|line| {
            let value = line.strip_prefix("version = \"")?.strip_suffix('"')?;
            Some(value.to_owned())
        })
        .expect("zmail-app's manifest has a version")
}

/// Draws `shot` on the test terminal with Zmail's version.
fn screen(shot: &Shot) -> ratatui::buffer::Buffer {
    let mut app = app();
    app.version = zmail_version();
    (shot.open)(&mut app, shot.width, shot.height);
    draw(&mut app, shot.width, shot.height)
}

/// Writes `<name>.png` for each screenshot named in
/// `ZMAIL_TUI_SCREENSHOTS` (all of them when unset) into
/// `ZMAIL_TUI_SCREENSHOT_DIR`, with the fonts in
/// `ZMAIL_TUI_SCREENSHOT_FONTS`. Run through
/// `scripts/capture-tui-screenshots.sh`, which sets all three.
#[test]
#[ignore = "needs the pinned fonts; run scripts/capture-tui-screenshots.sh"]
fn capture_documentation_screenshots() {
    let variable = |name: &str| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .unwrap_or_else(|| panic!("{name} is not set; run scripts/capture-tui-screenshots.sh"))
    };
    let out = variable("ZMAIL_TUI_SCREENSHOT_DIR");
    let font_dir = variable("ZMAIL_TUI_SCREENSHOT_FONTS");
    let wanted: Vec<String> = std::env::var("ZMAIL_TUI_SCREENSHOTS")
        .map(|names| names.split_whitespace().map(str::to_owned).collect())
        .unwrap_or_else(|_| {
            SCREENSHOTS
                .iter()
                .map(|shot| shot.name.to_owned())
                .collect()
        });
    let fonts: Vec<Vec<u8>> = FONTS
        .iter()
        .map(|name| {
            std::fs::read(font_dir.join(name))
                .unwrap_or_else(|error| panic!("font {name} in {}: {error}", font_dir.display()))
        })
        .collect();
    let mut renderer = Renderer::new(&fonts, FONT_SIZE, Palette::DARK).expect("the fonts load");
    std::fs::create_dir_all(&out).expect("the output folder");
    for name in &wanted {
        let shot = SCREENSHOTS
            .iter()
            .find(|shot| shot.name == name)
            .unwrap_or_else(|| panic!("no terminal UI screenshot {name}"));
        let rendered = renderer.render(&screen(shot));
        assert!(
            rendered.missing.is_empty(),
            "{name}: no font has {:?}",
            rendered.missing
        );
        let path = out.join(format!("{name}.png"));
        std::fs::write(&path, rendered.image.png()).expect("the screenshot is written");
        eprintln!(
            "{}: {}x{} cells, {}x{} pixels",
            path.display(),
            shot.width,
            shot.height,
            rendered.image.width,
            rendered.image.height
        );
    }
}

/// The screenshots between the `# tui-states` markers of
/// `scripts/capture-tui-screenshots.sh`, which the documentation checks
/// (`capture-screenshots.sh --check`) read without building anything.
fn script_states() -> Vec<String> {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/capture-tui-screenshots.sh");
    let script = std::fs::read_to_string(&path).expect("the capture script");
    script
        .lines()
        .skip_while(|line| line.trim() != "# tui-states: begin")
        .skip(1)
        .take_while(|line| line.trim() != "# tui-states: end")
        .map(|line| line.trim().to_owned())
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

#[test]
fn the_capture_script_lists_every_screenshot() {
    let names: Vec<&str> = SCREENSHOTS.iter().map(|shot| shot.name).collect();
    assert_eq!(script_states(), names);
    for name in names {
        assert!(
            name.starts_with("tui-"),
            "{name}: docs/site.md's naming rule"
        );
    }
}

#[test]
fn the_screenshots_show_the_release_version_and_synthetic_mail_only() {
    let version = zmail_version();
    assert!(
        version.split('.').count() == 3 && version != "0.0.0",
        "{version}"
    );
    for shot in SCREENSHOTS {
        let text = super::text_of(&screen(shot));
        assert!(
            text.lines()
                .next()
                .is_some_and(|title| title.contains(&format!("ZMAIL {version}"))),
            "{}:\n{text}",
            shot.name
        );
        // Every address on screen is the fixture's (.example), never a
        // real one. (A lone @ is the attachment column.)
        for word in text.split(|ch: char| ch.is_whitespace() || "<>()[],".contains(ch)) {
            if let Some((_, domain)) = word.split_once('@').filter(|(user, _)| !user.is_empty()) {
                assert!(domain.ends_with(".example"), "{}: {word}", shot.name);
            }
        }
    }
}
