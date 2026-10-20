//! `Avatar`: a sender's initials on a colour derived from their address.
//!
//! The colour is a stable hash of the address (or the name when there is
//! no address), so a sender keeps the same colour across runs, machines
//! and Rust versions. Images (BIMI, Gravatar, contacts) come later.
//!
//! Avatars are decorative: the row or message around one names the
//! sender, so the avatar has no accessibility node.

use gpui::{
    App, FontWeight, IntoElement, RenderOnce, SharedString, Window, div, prelude::*, px, rgb,
};

use crate::styles::contrast::readable_on;
use crate::styles::{AVATAR_SIZE, RADIUS_FULL};

use super::theme::ComponentTheme;

/// Avatar fills. Each is dark enough for white initials at 4.5:1 and
/// light enough to stand out from both bundled themes' backgrounds.
pub(crate) const AVATAR_PALETTE: [u32; 8] = [
    0x1D4ED8, // blue
    0x6D28D9, // violet
    0xBE185D, // pink
    0xB91C1C, // red
    0x9A3412, // orange
    0x15803D, // green
    0x0F766E, // teal
    0x4338CA, // indigo
];

/// A circular avatar with a sender's initials.
#[derive(IntoElement)]
pub(crate) struct Avatar {
    name: SharedString,
    address: Option<SharedString>,
    size: f32,
}

impl Avatar {
    /// An avatar for a sender called `name`.
    pub(crate) fn new(name: impl Into<SharedString>) -> Self {
        Self {
            name: name.into(),
            address: None,
            size: AVATAR_SIZE,
        }
    }

    /// The sender's address: it picks the colour, and the initials when
    /// the name has none.
    pub(crate) fn address(mut self, address: impl Into<SharedString>) -> Self {
        self.address = Some(address.into());
        self
    }

    /// The diameter in pixels (default [`AVATAR_SIZE`]).
    pub(crate) fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }
}

impl RenderOnce for Avatar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = ComponentTheme::get(cx);
        let address = self.address.as_deref();
        let fill = avatar_color(&self.name, address);
        let text = readable_on(fill, &[0xFFFFFF, theme.colors.foreground]);
        div()
            .flex()
            // Never squeezed by a long name beside it.
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(self.size))
            .rounded(px(RADIUS_FULL))
            .bg(rgb(fill))
            .text_size(px((self.size * 0.4).round()))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(rgb(text))
            .child(initials(&self.name, address))
    }
}

/// The avatar fill for a sender: a stable hash of the address when there
/// is one, otherwise of the name.
pub(crate) fn avatar_color(name: &str, address: Option<&str>) -> u32 {
    let seed = address
        .map(str::trim)
        .filter(|address| !address.is_empty())
        .unwrap_or(name)
        .to_lowercase();
    let index = fnv1a(seed.as_bytes()) as usize % AVATAR_PALETTE.len();
    AVATAR_PALETTE[index]
}

/// 32-bit FNV-1a: tiny and, unlike `DefaultHasher`, stable forever.
fn fnv1a(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811C_9DC5_u32, |hash, byte| {
        (hash ^ u32::from(*byte)).wrapping_mul(0x0100_0193)
    })
}

/// The initials for a sender.
///
/// - Two letters (first and last word) for names in cased scripts:
///   "Ada Lovelace" → "AL", "Doe, Jane" → "JD".
/// - One letter for a single word or an uncased script (CJK, Arabic,
///   Hebrew, Thai…), where first-plus-last does not make sense:
///   "Cher" → "C", "张伟" → "张".
/// - Emoji, punctuation and "(Work)"-style suffixes are skipped.
/// - With no usable name, the address's local part is used
///   ("jane.doe@example.com" → "JD"), and failing that "?".
pub(crate) fn initials(name: &str, address: Option<&str>) -> String {
    let name = name.trim().trim_matches(|ch| matches!(ch, '"' | '\''));
    // A bare address in the name field.
    let from_name = if name.contains('@') && !name.contains(char::is_whitespace) {
        None
    } else {
        initials_from_words(&display_order(&strip_parenthesised(name)))
    };
    from_name
        .or_else(|| {
            let address = if name.contains('@') {
                Some(name)
            } else {
                address
            };
            let local = address?.split('@').next()?;
            let words = local.replace(['.', '_', '-', '+'], " ");
            initials_from_words(&words)
        })
        .unwrap_or_else(|| "?".to_owned())
}

/// Drop "(…)" and "[…]" segments: "Jane Doe (Work)" → "Jane Doe".
fn strip_parenthesised(name: &str) -> String {
    let mut depth = 0usize;
    name.chars()
        .filter(|ch| match ch {
            '(' | '[' => {
                depth += 1;
                false
            }
            ')' | ']' => {
                depth = depth.saturating_sub(1);
                false
            }
            _ => depth == 0,
        })
        .collect()
}

/// "Doe, Jane" → "Jane Doe". Names with more than one comma are left
/// alone.
fn display_order(name: &str) -> String {
    match name.split_once(',') {
        Some((last, first)) if !first.contains(',') && !first.trim().is_empty() => {
            format!("{} {}", first.trim(), last.trim())
        }
        _ => name.to_owned(),
    }
}

/// The first letter or digit of a word, if it has one.
fn leading_letter(word: &str) -> Option<char> {
    word.chars().find(|ch| ch.is_alphanumeric())
}

/// Whether `ch` belongs to a script with upper and lower case.
fn is_cased(ch: char) -> bool {
    ch.is_uppercase() || ch.is_lowercase()
}

fn initials_from_words(text: &str) -> Option<String> {
    let letters: Vec<char> = text.split_whitespace().filter_map(leading_letter).collect();
    let first = *letters.first()?;
    let mut result = upper(first);
    if letters.len() > 1 && is_cased(first) {
        let last = letters[letters.len() - 1];
        if is_cased(last) {
            result.push_str(&upper(last));
        }
    }
    Some(result)
}

/// Upper-case one letter, keeping it as is when that would expand it
/// ("ß" would become "SS").
fn upper(ch: char) -> String {
    let mut upper = ch.to_uppercase();
    match (upper.next(), upper.next()) {
        (Some(single), None) => single.to_string(),
        _ => ch.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::styles::contrast::{MIN_TEXT_CONTRAST, contrast_ratio};

    #[test]
    fn latin_names_use_first_and_last_initials() {
        assert_eq!(initials("Ada Lovelace", None), "AL");
        assert_eq!(initials("ada lovelace", None), "AL");
        assert_eq!(initials("Ana María López García", None), "AG");
        assert_eq!(initials("  Ada   Lovelace  ", None), "AL");
    }

    #[test]
    fn single_names_use_one_letter() {
        assert_eq!(initials("Cher", None), "C");
        assert_eq!(initials("\"Madonna\"", None), "M");
    }

    #[test]
    fn surname_first_and_suffixes() {
        assert_eq!(initials("Doe, Jane", None), "JD");
        assert_eq!(initials("Jane Doe (Work)", None), "JD");
        assert_eq!(initials("Support [Zmail]", None), "S");
    }

    #[test]
    fn emoji_and_punctuation_are_skipped() {
        assert_eq!(initials("🦄 Unicorn Ops", None), "UO");
        assert_eq!(initials("Rob 🚀", None), "R");
        assert_eq!(initials("— Jane —", None), "J");
        assert_eq!(initials("🦄", Some("unicorn@example.com")), "U");
        assert_eq!(initials("🦄🚀", None), "?");
    }

    #[test]
    fn non_latin_scripts() {
        // Uncased scripts get one character.
        assert_eq!(initials("张伟", None), "张");
        assert_eq!(initials("山田 太郎", None), "山");
        assert_eq!(initials("김철수", None), "김");
        assert_eq!(initials("محمد علي", None), "م");
        // Cased non-Latin scripts work like Latin.
        assert_eq!(initials("Дмитрий Иванов", None), "ДИ");
        assert_eq!(initials("όμηρος παππάς", None), "ΌΠ");
        // Accents survive, decomposed or not.
        assert_eq!(initials("Émile Zola", None), "ÉZ");
        assert_eq!(initials("E\u{301}mile Zola", None), "EZ");
        // Letters that expand when upper-cased stay as they are.
        assert_eq!(initials("ßeta", None), "ß");
    }

    #[test]
    fn addresses_are_the_fallback() {
        assert_eq!(initials("", Some("jane.doe@example.com")), "JD");
        assert_eq!(initials("jane_doe@example.com", None), "JD");
        assert_eq!(initials("", Some("billing+eu@example.com")), "BE");
        assert_eq!(initials("", Some("42@example.com")), "4");
        assert_eq!(initials("", None), "?");
    }

    #[test]
    fn colour_is_stable_and_keyed_on_the_address() {
        let first = avatar_color("Jane", Some("Jane@Example.com"));
        assert_eq!(
            first,
            avatar_color("Someone Else", Some("jane@example.com"))
        );
        assert_eq!(first, avatar_color("Jane", Some(" jane@example.com ")));
        assert_eq!(avatar_color("Jane", None), avatar_color("jane", None));
        // Pinned so a refactor cannot silently recolour every sender.
        assert_eq!(fnv1a(b"jane@example.com"), 0x3059_8ACC);
    }

    #[test]
    fn white_initials_are_readable_on_every_palette_colour() {
        for fill in AVATAR_PALETTE {
            let ratio = contrast_ratio(0xFFFFFF, fill);
            assert!(ratio >= MIN_TEXT_CONTRAST, "#{fill:06X}: {ratio:.2}:1");
        }
    }
}
