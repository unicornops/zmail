//! Zmail's own scripts in a page, and what they report.
//!
//! Page JavaScript is always off. The engines still run scripts the app
//! injects (WebKit in its isolated client world, WebView2 through
//! `ExecuteScript`, which page scripts being off does not stop): Zmail
//! uses that to measure a page and, on Windows, for Copy and Select All.
//! Nothing here is platform specific, so it is tested on every OS.

/// Measures the page: its content height and scroll width, and the
/// viewport width, all in CSS pixels, as `"height scrollWidth viewportWidth"`.
pub(crate) const MEASURE_SCRIPT: &str = "(() => { \
    const d = document.documentElement, b = document.body; \
    let h = d.getBoundingClientRect().height; \
    if (b) { \
        const s = getComputedStyle(b); \
        h = Math.max(h, b.getBoundingClientRect().bottom + (parseFloat(s.marginBottom) || 0)); \
    } \
    const w = Math.max(d.scrollWidth, b ? b.scrollWidth : 0); \
    return Math.ceil(h) + ' ' + Math.ceil(w) + ' ' + Math.floor(window.innerWidth); \
})()";

/// The page's selected text.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) const SELECTION_SCRIPT: &str = "String(window.getSelection() || '')";

/// Selects the whole page.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) const SELECT_ALL_SCRIPT: &str = "(() => { const b = document.body; if (b) { window.getSelection().selectAllChildren(b); } return ''; })()";

/// The smallest zoom used to fit a page that is wider than the pane.
pub(crate) const MIN_FIT_ZOOM: f64 = 0.5;

/// The tallest a page is shown, in points; taller mail is cut off.
pub(crate) const MAX_HEIGHT: f64 = 40_000.;

/// What [`MEASURE_SCRIPT`] reported, in CSS pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Measurement {
    /// The content's height.
    pub height: f64,
    /// The content's width, including what overflows the viewport.
    pub scroll_width: f64,
    /// The viewport's width.
    pub viewport_width: f64,
}

impl Measurement {
    /// Parses the script's `"height scrollWidth viewportWidth"`.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let numbers: Vec<f64> = text
            .split_whitespace()
            .filter_map(|part| part.parse().ok())
            .collect();
        let [height, scroll_width, viewport_width] = numbers[..] else {
            return None;
        };
        Some(Self {
            height,
            scroll_width,
            viewport_width,
        })
    }

    /// The zoom that fits a page wider than its viewport, or `None` when it
    /// fits already.
    pub(crate) fn fit_zoom(&self) -> Option<f64> {
        (self.viewport_width > 0. && self.scroll_width > self.viewport_width + 1.)
            .then(|| (self.viewport_width / self.scroll_width).clamp(MIN_FIT_ZOOM, 1.))
    }

    /// The height to show at `zoom`, in points, capped at [`MAX_HEIGHT`].
    pub(crate) fn points(&self, zoom: f64) -> f64 {
        (self.height * zoom).ceil().clamp(1., MAX_HEIGHT)
    }
}

/// Decodes a JSON string literal, such as WebView2's `ExecuteScript`
/// result for a script that returns a string. Anything else (`null`, a
/// number, malformed JSON) is `None`.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) fn decode_json_string(json: &str) -> Option<String> {
    let inner = json.trim().strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch == '"' {
            return None;
        }
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next()? {
            '"' => out.push('"'),
            '\\' => out.push('\\'),
            '/' => out.push('/'),
            'b' => out.push('\u{8}'),
            'f' => out.push('\u{c}'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'u' => {
                let unit = hex4(&mut chars)?;
                let code = if (0xd800..0xdc00).contains(&unit) {
                    // A surrogate pair: the low half must follow.
                    if chars.next()? != '\\' || chars.next()? != 'u' {
                        return None;
                    }
                    let low = hex4(&mut chars)?;
                    if !(0xdc00..0xe000).contains(&low) {
                        return None;
                    }
                    0x10000 + ((unit - 0xd800) << 10) + (low - 0xdc00)
                } else {
                    unit
                };
                out.push(char::from_u32(code)?);
            }
            _ => return None,
        }
    }
    Some(out)
}

fn hex4(chars: &mut std::str::Chars<'_>) -> Option<u32> {
    let digits: String = chars.take(4).collect();
    if digits.len() != 4 {
        return None;
    }
    u32::from_str_radix(&digits, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_measurement_is_three_numbers() {
        assert_eq!(
            Measurement::parse("1234 600 600"),
            Some(Measurement {
                height: 1234.,
                scroll_width: 600.,
                viewport_width: 600.
            })
        );
        assert_eq!(Measurement::parse("1234 600"), None);
        assert_eq!(Measurement::parse("tall wide"), None);
    }

    #[test]
    fn only_a_page_wider_than_its_viewport_is_zoomed() {
        let fits = Measurement::parse("800 600 600").expect("parses");
        assert_eq!(fits.fit_zoom(), None);
        let wide = Measurement::parse("800 1200 600").expect("parses");
        assert_eq!(wide.fit_zoom(), Some(0.5));
        let very_wide = Measurement::parse("800 4000 600").expect("parses");
        assert_eq!(very_wide.fit_zoom(), Some(MIN_FIT_ZOOM));
        assert_eq!(wide.points(0.5), 400.);
        assert_eq!(
            Measurement::parse("900000 600 600")
                .expect("parses")
                .points(1.),
            MAX_HEIGHT
        );
    }

    #[test]
    fn json_strings_are_decoded() {
        assert_eq!(decode_json_string("\"safe\""), Some("safe".to_owned()));
        assert_eq!(
            decode_json_string(r#""a \"quote\"\né😀\\""#),
            Some("a \"quote\"\n\u{e9}\u{1f600}\\".to_owned())
        );
        assert_eq!(decode_json_string("null"), None);
        assert_eq!(decode_json_string("12"), None);
        assert_eq!(decode_json_string(r#""bad \x""#), None);
        assert_eq!(decode_json_string(r#""lone \ud83d""#), None);
        assert_eq!(decode_json_string(r#""un"quoted""#), None);
    }
}
