//! A terminal screen as a PNG: the test terminal's buffer, cell by cell,
//! with its colours and modifiers, for the documentation's screenshots of
//! the terminal UI (`screenshots.rs`, docs/site.md).
//!
//! Every character is placed by its cell, never by the font's advance, so
//! the columns line up whatever the font: a wide character (CJK, emoji)
//! takes exactly two cells and is centred in them. Glyphs come from the
//! given fonts in order (the first one that has the character), drawn
//! unhinted with swash; the box-drawing lines are drawn as rectangles so
//! they join across cells. The colours are [`Palette::DARK`]: a real
//! terminal's differ, so this is one fixed, documented choice. The output
//! depends only on the buffer, the fonts and this code: the same input
//! gives the same bytes.

use std::collections::BTreeSet;

use ratatui::{
    buffer::Buffer,
    style::{Color, Modifier},
};
use swash::{
    FontRef,
    scale::{Render, ScaleContext, Source, image::Content},
    zeno::{Angle, Format, Transform},
};

use crate::text;

/// An sRGB colour.
pub type Rgb = [u8; 3];

/// The colours a terminal draws with: its default foreground and
/// background (ratatui's `Color::Reset`) and the 16 ANSI colours, in
/// ANSI order (black, red, green, yellow, blue, magenta, cyan, white, then
/// the bright eight). ratatui's `Gray` is ANSI 7 and `DarkGray` ANSI 8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub foreground: Rgb,
    pub background: Rgb,
    pub ansi: [Rgb; 16],
}

impl Palette {
    /// A dark terminal in the colours of Zmail's dark theme
    /// (`themes/phase0-dark.json`: its background and text), with ANSI
    /// colours picked to read on that background. It reads on the
    /// documentation's light and dark pages alike, as a terminal window.
    pub const DARK: Self = Self {
        foreground: [0xE5, 0xEE, 0xF9],
        background: [0x11, 0x18, 0x27],
        ansi: [
            [0x1F, 0x29, 0x37], // black
            [0xF8, 0x71, 0x71], // red
            [0x34, 0xD3, 0x99], // green
            [0xFB, 0xBF, 0x24], // yellow
            [0x60, 0xA5, 0xFA], // blue
            [0xC0, 0x84, 0xFC], // magenta
            [0x22, 0xD3, 0xEE], // cyan
            [0xCB, 0xD5, 0xE1], // white (ratatui's Gray)
            [0x7D, 0x8B, 0xA1], // bright black (ratatui's DarkGray)
            [0xFC, 0xA5, 0xA5], // bright red
            [0x6E, 0xE7, 0xB7], // bright green
            [0xFD, 0xE6, 0x8A], // bright yellow
            [0x93, 0xC5, 0xFD], // bright blue
            [0xD8, 0xB4, 0xFE], // bright magenta
            [0x67, 0xE8, 0xF9], // bright cyan
            [0xF8, 0xFA, 0xFC], // bright white (ratatui's White)
        ],
    };

    /// `color` as RGB, with `default` for `Color::Reset`. Indexed colours
    /// past 15 are xterm's 6x6x6 cube and grey ramp.
    pub fn rgb(&self, color: Color, default: Rgb) -> Rgb {
        let ansi = |index: usize| self.ansi[index];
        match color {
            Color::Reset => default,
            Color::Black => ansi(0),
            Color::Red => ansi(1),
            Color::Green => ansi(2),
            Color::Yellow => ansi(3),
            Color::Blue => ansi(4),
            Color::Magenta => ansi(5),
            Color::Cyan => ansi(6),
            Color::Gray => ansi(7),
            Color::DarkGray => ansi(8),
            Color::LightRed => ansi(9),
            Color::LightGreen => ansi(10),
            Color::LightYellow => ansi(11),
            Color::LightBlue => ansi(12),
            Color::LightMagenta => ansi(13),
            Color::LightCyan => ansi(14),
            Color::White => ansi(15),
            Color::Rgb(red, green, blue) => [red, green, blue],
            Color::Indexed(index @ 0..=15) => ansi(usize::from(index)),
            Color::Indexed(index @ 16..=231) => {
                let level = |step: u8| if step == 0 { 0 } else { 55 + step * 40 };
                let index = index - 16;
                [level(index / 36), level(index / 6 % 6), level(index % 6)]
            }
            Color::Indexed(index) => {
                let grey = 8 + (index - 232) * 10;
                [grey, grey, grey]
            }
        }
    }
}

/// An RGB image, three bytes a pixel, row by row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl Image {
    fn new(width: u32, height: u32, fill: Rgb) -> Self {
        Self {
            width,
            height,
            pixels: fill.repeat((width * height) as usize),
        }
    }

    pub fn pixel(&self, x: u32, y: u32) -> Rgb {
        let at = ((y * self.width + x) * 3) as usize;
        [self.pixels[at], self.pixels[at + 1], self.pixels[at + 2]]
    }

    /// Fills the rectangle, clipped to the image.
    fn fill(&mut self, x: i64, y: i64, width: i64, height: i64, color: Rgb) {
        for row in y.max(0)..(y + height).min(i64::from(self.height)) {
            for column in x.max(0)..(x + width).min(i64::from(self.width)) {
                let at = ((row * i64::from(self.width) + column) * 3) as usize;
                self.pixels[at..at + 3].copy_from_slice(&color);
            }
        }
    }

    /// Paints `color` at `coverage` (0 to 255) over the pixel, if inside.
    fn blend(&mut self, x: i64, y: i64, color: Rgb, coverage: u8) {
        let inside =
            (0..i64::from(self.width)).contains(&x) && (0..i64::from(self.height)).contains(&y);
        if coverage == 0 || !inside {
            return;
        }
        let at = ((y * i64::from(self.width) + x) * 3) as usize;
        for (channel, value) in color.iter().enumerate() {
            let under = u32::from(self.pixels[at + channel]);
            let over = u32::from(*value);
            let alpha = u32::from(coverage);
            self.pixels[at + channel] = ((over * alpha + under * (255 - alpha) + 127) / 255) as u8;
        }
    }

    /// The image as an 8-bit RGB PNG with no metadata, so the bytes depend
    /// only on the pixels.
    pub fn png(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, self.width, self.height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::High);
        let mut writer = encoder.write_header().expect("the PNG header is written");
        writer
            .write_image_data(&self.pixels)
            .expect("the PNG data is written");
        writer.finish().expect("the PNG is finished");
        out
    }
}

/// A rendered screen, and the characters no font had (drawn as nothing).
#[derive(Debug)]
pub struct Rendered {
    pub image: Image,
    pub missing: BTreeSet<char>,
}

/// The size of the text and of the cells, in pixels.
#[derive(Debug, Clone, Copy)]
pub struct Metrics {
    /// The font size (pixels per em).
    pub size: f32,
    pub cell_width: u32,
    pub cell_height: u32,
    /// From the top of a cell to the baseline.
    pub baseline: u32,
    /// From the baseline down to the top of an underline.
    pub underline: u32,
    /// Underline and box-drawing line thickness.
    pub stroke: u32,
    /// The empty border round the screen.
    pub padding: u32,
}

/// Draws buffers with `fonts` (font files' bytes, the first one the main,
/// monospaced font, the rest fallbacks for what it lacks).
pub struct Renderer<'a> {
    fonts: Vec<FontRef<'a>>,
    palette: Palette,
    metrics: Metrics,
    context: ScaleContext,
}

/// Bold glyphs: the weight axis of a variable font. A font without one
/// draws bold the same as regular.
const BOLD_WEIGHT: f32 = 700.0;
const REGULAR_WEIGHT: f32 = 400.0;
/// Italic is the regular glyph slanted, as many terminals draw it.
const ITALIC_SLANT_DEGREES: f32 = 12.0;

impl<'a> Renderer<'a> {
    /// A renderer at `size` pixels per em. Fails when a font cannot be
    /// read.
    pub fn new(fonts: &'a [Vec<u8>], size: f32, palette: Palette) -> Result<Self, String> {
        let fonts = fonts
            .iter()
            .enumerate()
            .map(|(index, data)| {
                FontRef::from_index(data, 0).ok_or_else(|| format!("font {index} is not a font"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let main = fonts.first().ok_or("no fonts")?;
        let font_metrics = main.metrics(&[]).scale(size);
        let glyph_metrics = main.glyph_metrics(&[]).scale(size);
        let cell_width = glyph_metrics
            .advance_width(main.charmap().map('M'))
            .round()
            .max(1.0) as u32;
        let text_height = font_metrics.ascent + font_metrics.descent;
        let cell_height = (text_height + font_metrics.leading).ceil().max(1.0) as u32;
        let baseline = ((cell_height as f32 - text_height) / 2.0 + font_metrics.ascent).round();
        let stroke = font_metrics.stroke_size.round().max(1.0) as u32;
        // swash's offset is up from the baseline; underlines sit below it.
        let underline = (-font_metrics.underline_offset).round().max(1.0) as u32;
        Ok(Self {
            fonts,
            palette,
            metrics: Metrics {
                size,
                cell_width,
                cell_height,
                baseline: baseline as u32,
                underline,
                stroke,
                padding: cell_width,
            },
            context: ScaleContext::new(),
        })
    }

    pub fn metrics(&self) -> Metrics {
        self.metrics
    }

    /// Draws every cell of `buffer`.
    pub fn render(&mut self, buffer: &Buffer) -> Rendered {
        let metrics = self.metrics;
        let area = buffer.area;
        let mut image = Image::new(
            u32::from(area.width) * metrics.cell_width + 2 * metrics.padding,
            u32::from(area.height) * metrics.cell_height + 2 * metrics.padding,
            self.palette.background,
        );
        let mut missing = BTreeSet::new();
        for y in 0..area.height {
            let mut x = 0;
            while x < area.width {
                let cell = &buffer[(area.x + x, area.y + y)];
                // A wide character's cells are its own, as in a terminal:
                // the cell after it holds nothing to draw.
                let cells = text::width(cell.symbol()).clamp(1, usize::from(area.width - x));
                let (left, top) = (
                    i64::from(metrics.padding + u32::from(x) * metrics.cell_width),
                    i64::from(metrics.padding + u32::from(y) * metrics.cell_height),
                );
                let span = cells as u32 * metrics.cell_width;
                let (foreground, background) = self.colors(cell.fg, cell.bg, cell.modifier);
                image.fill(
                    left,
                    top,
                    i64::from(span),
                    i64::from(metrics.cell_height),
                    background,
                );
                if !cell.modifier.contains(Modifier::HIDDEN) {
                    self.glyph(
                        &mut image,
                        cell.symbol(),
                        cell.modifier,
                        (left, top, span),
                        foreground,
                        &mut missing,
                    );
                    let line = |image: &mut Image, from_top: u32| {
                        image.fill(
                            left,
                            top + i64::from(from_top),
                            i64::from(span),
                            i64::from(metrics.stroke),
                            foreground,
                        );
                    };
                    if cell.modifier.contains(Modifier::UNDERLINED) {
                        line(&mut image, metrics.baseline + metrics.underline);
                    }
                    if cell.modifier.contains(Modifier::CROSSED_OUT) {
                        line(&mut image, metrics.cell_height / 2);
                    }
                }
                x += cells as u16;
            }
        }
        Rendered { image, missing }
    }

    /// The cell's foreground and background after reverse video and dim.
    fn colors(&self, fg: Color, bg: Color, modifier: Modifier) -> (Rgb, Rgb) {
        let palette = self.palette;
        let mut foreground = palette.rgb(fg, palette.foreground);
        let mut background = palette.rgb(bg, palette.background);
        if modifier.contains(Modifier::REVERSED) {
            std::mem::swap(&mut foreground, &mut background);
        }
        if modifier.contains(Modifier::DIM) {
            foreground = mix(foreground, background);
        }
        (foreground, background)
    }

    /// Draws `symbol`'s glyph centred in the cells at `left`..`left + span`.
    fn glyph(
        &mut self,
        image: &mut Image,
        symbol: &str,
        modifier: Modifier,
        (left, top, span): (i64, i64, u32),
        color: Rgb,
        missing: &mut BTreeSet<char>,
    ) {
        // The grapheme's first character; variation selectors and joiners
        // after it change nothing here.
        let Some(ch) = symbol.chars().next() else {
            return;
        };
        if ch == ' ' {
            return;
        }
        let metrics = self.metrics;
        if box_drawing(image, ch, (left, top), metrics, color) {
            return;
        }
        let Some((font, glyph)) = self
            .fonts
            .iter()
            .find_map(|font| Some((*font, font.charmap().map(ch))).filter(|(_, id)| *id != 0))
        else {
            missing.insert(ch);
            return;
        };
        // Centred by its advance; shrunk when that is wider than its cells
        // (an emoji in a font whose em is wider than two cells).
        let advance = font
            .glyph_metrics(&[])
            .scale(metrics.size)
            .advance_width(glyph);
        let size = if advance > span as f32 && advance > 0.0 {
            metrics.size * span as f32 / advance
        } else {
            metrics.size
        };
        let advance = advance.min(span as f32);
        let weight = if modifier.contains(Modifier::BOLD) {
            BOLD_WEIGHT
        } else {
            REGULAR_WEIGHT
        };
        let mut scaler = self
            .context
            .builder(font)
            .size(size)
            .hint(false)
            .variations(&[("wght", weight)])
            .build();
        let slant = modifier
            .contains(Modifier::ITALIC)
            .then(|| Transform::skew(Angle::from_degrees(ITALIC_SLANT_DEGREES), Angle::ZERO));
        let Some(rendered) = Render::new(&[Source::Outline])
            .format(Format::Alpha)
            .transform(slant)
            .render(&mut scaler, glyph)
        else {
            return;
        };
        if rendered.content != Content::Mask {
            return;
        }
        let origin_x = left + ((span as f32 - advance) / 2.0).round() as i64;
        let baseline = top + i64::from(metrics.baseline);
        let placement = rendered.placement;
        for row in 0..placement.height {
            for column in 0..placement.width {
                let coverage = rendered.data[(row * placement.width + column) as usize];
                image.blend(
                    origin_x + i64::from(placement.left) + i64::from(column),
                    baseline - i64::from(placement.top) + i64::from(row),
                    color,
                    coverage,
                );
            }
        }
    }
}

/// Half way between `a` and `b`.
fn mix(a: Rgb, b: Rgb) -> Rgb {
    [0, 1, 2].map(|i| ((u16::from(a[i]) + u16::from(b[i])) / 2) as u8)
}

/// Draws the light box-drawing characters as lines through the cell's
/// centre, out to its edges, so they meet the next cell's; false for any
/// other character.
fn box_drawing(
    image: &mut Image,
    ch: char,
    (left, top): (i64, i64),
    metrics: Metrics,
    color: Rgb,
) -> bool {
    // Which arms each character has: left, right, up, down.
    let (west, east, north, south) = match ch {
        '─' => (true, true, false, false),
        '│' => (false, false, true, true),
        '┌' => (false, true, false, true),
        '┐' => (true, false, false, true),
        '└' => (false, true, true, false),
        '┘' => (true, false, true, false),
        '├' => (false, true, true, true),
        '┤' => (true, false, true, true),
        '┬' => (true, true, false, true),
        '┴' => (true, true, true, false),
        '┼' => (true, true, true, true),
        _ => return false,
    };
    let (width, height) = (
        i64::from(metrics.cell_width),
        i64::from(metrics.cell_height),
    );
    let stroke = i64::from(metrics.stroke);
    let (middle_x, middle_y) = (left + (width - stroke) / 2, top + (height - stroke) / 2);
    if west {
        image.fill(left, middle_y, middle_x - left + stroke, stroke, color);
    }
    if east {
        image.fill(middle_x, middle_y, left + width - middle_x, stroke, color);
    }
    if north {
        image.fill(middle_x, top, stroke, middle_y - top + stroke, color);
    }
    if south {
        image.fill(middle_x, middle_y, stroke, top + height - middle_y, color);
    }
    true
}

#[cfg(test)]
mod tests {
    use ratatui::{layout::Rect, style::Style};

    use super::*;

    /// Inter, the desktop app's font (in the repository): not monospaced
    /// and without CJK, which is what these tests need. The documentation
    /// screenshots use pinned fonts (`screenshots.rs`).
    fn fonts() -> Vec<Vec<u8>> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/fonts/inter/Inter-Regular.ttf");
        vec![std::fs::read(path).expect("Inter is in the repository")]
    }

    fn render(buffer: &Buffer) -> (Rendered, Metrics) {
        let fonts = fonts();
        let mut renderer = Renderer::new(&fonts, 20.0, Palette::DARK).expect("the font loads");
        (renderer.render(buffer), renderer.metrics())
    }

    /// The top-left pixel of cell (`x`, `y`): its background.
    fn corner(rendered: &Rendered, metrics: Metrics, x: u32, y: u32) -> Rgb {
        rendered.image.pixel(
            metrics.padding + x * metrics.cell_width,
            metrics.padding + y * metrics.cell_height,
        )
    }

    /// The colours found inside cell (`x`, `y`).
    fn colours_in(rendered: &Rendered, metrics: Metrics, x: u32, y: u32) -> BTreeSet<Rgb> {
        let mut found = BTreeSet::new();
        for py in 0..metrics.cell_height {
            for px in 0..metrics.cell_width {
                found.insert(rendered.image.pixel(
                    metrics.padding + x * metrics.cell_width + px,
                    metrics.padding + y * metrics.cell_height + py,
                ));
            }
        }
        found
    }

    #[test]
    fn the_image_is_the_grid_of_cells_with_a_border() {
        let buffer = Buffer::empty(Rect::new(0, 0, 12, 3));
        let (rendered, metrics) = render(&buffer);
        assert_eq!(
            (rendered.image.width, rendered.image.height),
            (
                12 * metrics.cell_width + 2 * metrics.padding,
                3 * metrics.cell_height + 2 * metrics.padding
            )
        );
        assert!(
            rendered
                .image
                .pixels
                .chunks(3)
                .all(|pixel| pixel == Palette::DARK.background),
            "an empty screen is all background"
        );
    }

    #[test]
    fn a_styled_cell_has_its_colours() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 4, 1));
        buffer.set_string(1, 0, "W", Style::new().red().on_blue().bold());
        let (rendered, metrics) = render(&buffer);
        let red = Palette::DARK.ansi[1];
        let blue = Palette::DARK.ansi[4];
        assert_eq!(corner(&rendered, metrics, 1, 0), blue);
        let inside = colours_in(&rendered, metrics, 1, 0);
        assert!(
            inside.contains(&red),
            "the glyph is drawn in red: {inside:?}"
        );
        assert!(inside.contains(&blue));
        assert_eq!(corner(&rendered, metrics, 0, 0), Palette::DARK.background);
        assert_eq!(
            colours_in(&rendered, metrics, 2, 0),
            BTreeSet::from([Palette::DARK.background]),
            "the next cell is untouched"
        );
    }

    #[test]
    fn reverse_video_swaps_the_default_colours() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 3, 1));
        buffer.set_string(0, 0, "a b", Style::new().reversed());
        let (rendered, metrics) = render(&buffer);
        for x in 0..3 {
            assert_eq!(corner(&rendered, metrics, x, 0), Palette::DARK.foreground);
        }
        assert!(colours_in(&rendered, metrics, 0, 0).contains(&Palette::DARK.background));
        assert_eq!(
            colours_in(&rendered, metrics, 1, 0),
            BTreeSet::from([Palette::DARK.foreground]),
            "a reversed space is a block of the foreground colour"
        );
    }

    #[test]
    fn a_wide_character_takes_two_cells() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 6, 1));
        buffer.set_string(0, 0, "東x", Style::new().on_green());
        buffer.set_string(4, 0, "y", Style::new());
        let (rendered, metrics) = render(&buffer);
        let green = Palette::DARK.ansi[2];
        assert_eq!(corner(&rendered, metrics, 0, 0), green);
        assert_eq!(
            corner(&rendered, metrics, 1, 0),
            green,
            "the second cell is the wide character's, as in a terminal"
        );
        assert_eq!(
            corner(&rendered, metrics, 2, 0),
            green,
            "x follows in cell 2"
        );
        assert!(colours_in(&rendered, metrics, 2, 0).contains(&Palette::DARK.foreground));
        assert_eq!(corner(&rendered, metrics, 3, 0), Palette::DARK.background);
        assert!(colours_in(&rendered, metrics, 4, 0).contains(&Palette::DARK.foreground));
        assert_eq!(
            rendered.missing,
            BTreeSet::from(['東']),
            "Inter has no CJK, and says so"
        );
    }

    #[test]
    fn box_drawing_lines_meet_across_cells() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 3, 3));
        buffer.set_string(0, 1, "─┼─", Style::new().cyan());
        buffer.set_string(1, 0, "│", Style::new().cyan());
        buffer.set_string(1, 2, "│", Style::new().cyan());
        let (rendered, metrics) = render(&buffer);
        let cyan = Palette::DARK.ansi[6];
        let middle_y =
            metrics.padding + metrics.cell_height + (metrics.cell_height - metrics.stroke) / 2;
        for x in 0..3 * metrics.cell_width {
            assert_eq!(rendered.image.pixel(metrics.padding + x, middle_y), cyan);
        }
        let middle_x =
            metrics.padding + metrics.cell_width + (metrics.cell_width - metrics.stroke) / 2;
        for y in 0..3 * metrics.cell_height {
            assert_eq!(rendered.image.pixel(middle_x, metrics.padding + y), cyan);
        }
        assert!(rendered.missing.is_empty());
    }

    #[test]
    fn underline_dim_and_hidden() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 3, 1));
        buffer.set_string(0, 0, "_", Style::new().underlined());
        buffer.set_string(1, 0, "M", Style::new().dim());
        buffer.set_string(2, 0, "M", Style::new().hidden());
        let (rendered, metrics) = render(&buffer);
        let underline_y = metrics.padding + metrics.baseline + metrics.underline;
        for x in 0..metrics.cell_width {
            assert_eq!(
                rendered.image.pixel(metrics.padding + x, underline_y),
                Palette::DARK.foreground
            );
        }
        let dim = mix(Palette::DARK.foreground, Palette::DARK.background);
        assert!(colours_in(&rendered, metrics, 1, 0).contains(&dim));
        assert!(!colours_in(&rendered, metrics, 1, 0).contains(&Palette::DARK.foreground));
        assert_eq!(
            colours_in(&rendered, metrics, 2, 0),
            BTreeSet::from([Palette::DARK.background])
        );
    }

    #[test]
    fn the_palette_is_fixed() {
        let palette = Palette::DARK;
        let reset = palette.rgb(Color::Reset, [1, 2, 3]);
        assert_eq!(reset, [1, 2, 3], "Reset is the default given");
        let named = [
            Color::Black,
            Color::Red,
            Color::Green,
            Color::Yellow,
            Color::Blue,
            Color::Magenta,
            Color::Cyan,
            Color::Gray,
            Color::DarkGray,
            Color::LightRed,
            Color::LightGreen,
            Color::LightYellow,
            Color::LightBlue,
            Color::LightMagenta,
            Color::LightCyan,
            Color::White,
        ];
        for (index, color) in named.into_iter().enumerate() {
            assert_eq!(palette.rgb(color, [0; 3]), palette.ansi[index], "{color:?}");
            assert_eq!(
                palette.rgb(Color::Indexed(index as u8), [0; 3]),
                palette.ansi[index]
            );
        }
        assert_eq!(palette.rgb(Color::Indexed(16), [9; 3]), [0, 0, 0]);
        assert_eq!(palette.rgb(Color::Indexed(196), [9; 3]), [255, 0, 0]);
        assert_eq!(palette.rgb(Color::Indexed(231), [9; 3]), [255, 255, 255]);
        assert_eq!(palette.rgb(Color::Indexed(232), [9; 3]), [8, 8, 8]);
        assert_eq!(palette.rgb(Color::Indexed(255), [9; 3]), [238, 238, 238]);
        assert_eq!(palette.rgb(Color::Rgb(1, 2, 3), [9; 3]), [1, 2, 3]);
        // The defaults and every colour are distinct, and the text reads.
        let mut all: Vec<Rgb> = palette.ansi.to_vec();
        all.extend([palette.foreground, palette.background]);
        assert_eq!(all.iter().collect::<BTreeSet<_>>().len(), all.len());
    }

    #[test]
    fn the_same_screen_gives_the_same_bytes() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 20, 2));
        buffer.set_string(0, 0, "Zmail ─ 東 ✓", Style::new().bold().italic());
        buffer.set_string(0, 1, "N  Grace Hopper", Style::new().reversed());
        let first = render(&buffer).0.image.png();
        let second = render(&buffer).0.image.png();
        assert_eq!(first, second);
        // And the PNG is the image.
        let mut decoder = png::Decoder::new(std::io::Cursor::new(&first))
            .read_info()
            .expect("a PNG");
        let mut pixels = vec![0; decoder.output_buffer_size().expect("a size")];
        let info = decoder.next_frame(&mut pixels).expect("its pixels");
        let image = render(&buffer).0.image;
        assert_eq!((info.width, info.height), (image.width, image.height));
        assert_eq!(pixels, image.pixels);
    }
}
