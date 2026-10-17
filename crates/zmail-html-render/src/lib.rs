//! Spike for issue #134: render sanitised HTML mail off-screen with Blitz.
//!
//! This crate is evaluation code, not a reader feature. Nothing in the app
//! depends on it. It answers three questions for the go/no-go write-up in
//! `docs/spikes/blitz-html-renderer.md`:
//!
//! - does Blitz build next to the pinned GPUI with our toolchain,
//! - how faithfully and how fast does it render real-world mail HTML, and
//! - can every resource load be forced through a zmail-owned provider.
//!
//! [`render`] parses the HTML, resolves style and layout at a fixed CSS
//! width, and rasterises the full document height with the CPU renderer
//! (`vello_cpu`) into an RGBA buffer. The returned [`RenderedMail`] keeps the
//! laid-out document so a click on the image can be mapped back to a link
//! with [`RenderedMail::link_at`].

mod provider;

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use anyrender::{PaintScene as _, render_to_buffer};
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::{BaseDocument, DocumentConfig, StyleThreading, local_name};
use blitz_html::DocumentHtmlParser;
use blitz_traits::shell::{ColorScheme, Viewport};
use html5ever::{
    ParseOpts,
    tendril::TendrilSink as _,
    tokenizer::TokenizerOpts,
    tree_builder::{QuirksMode, TreeBuilderOpts},
};
use peniko::{Color, Fill, kurbo::Rect};

pub use provider::{InlinePart, MailResources, Refusal, RemoteFetcher, RemotePolicy};

/// Base URL relative references resolve against. Not a real origin: it only
/// gives relative `src`/`href` values a scheme the provider refuses.
const MAIL_BASE_URL: &str = "zmail-message://local/";

/// Viewport height (CSS px) used while measuring the document.
const MEASURE_HEIGHT: u32 = 100;

/// Parses mail HTML with the HTML parser, never the XML one.
///
/// `HtmlDocument::from_html` switches to xml5ever when the document carries
/// an XHTML doctype or namespace, which most bulk-mail templates do. Mail is
/// `text/html` and almost never well-formed XML, so that path yields an
/// empty document; browsers and mail clients always use the HTML parser.
fn parse_html(document: &mut BaseDocument, html: &str) -> Result<()> {
    let mut mutator = document.mutate();
    let mut sink = DocumentHtmlParser::new(&mut mutator);
    sink.is_xml = false;
    let opts = ParseOpts {
        tokenizer: TokenizerOpts::default(),
        tree_builder: TreeBuilderOpts {
            exact_errors: false,
            // No script engine, so `<noscript>` content is shown.
            scripting_enabled: false,
            iframe_srcdoc: false,
            drop_doctype: true,
            quirks_mode: QuirksMode::NoQuirks,
        },
    };
    html5ever::parse_document(sink, opts)
        .from_utf8()
        .read_from(&mut html.as_bytes())?;
    Ok(())
}

/// How to render one message.
#[derive(Debug, Clone, Copy)]
pub struct RenderOptions {
    /// Layout width in CSS pixels (the reader pane width).
    pub width: u32,
    /// Device pixel ratio of the output image.
    pub scale: f32,
    /// Answers `prefers-color-scheme` media queries.
    pub dark: bool,
    /// Tallest document rendered, in CSS pixels; the rest is cut off.
    pub max_height: u32,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            width: 800,
            scale: 1.0,
            dark: false,
            max_height: 20_000,
        }
    }
}

/// Wall-clock time spent in each phase of one render.
#[derive(Debug, Clone, Copy, Default)]
pub struct Timings {
    /// HTML parse and DOM construction (including resource callbacks).
    pub parse: Duration,
    /// Style resolution and layout.
    pub layout: Duration,
    /// Scene painting and CPU rasterisation.
    pub raster: Duration,
}

impl Timings {
    /// Sum of all phases.
    pub fn total(&self) -> Duration {
        self.parse + self.layout + self.raster
    }
}

/// A rendered message: pixels plus the document for hit-testing.
pub struct RenderedMail {
    document: BaseDocument,
    /// Image width in device pixels.
    pub pixel_width: u32,
    /// Image height in device pixels.
    pub pixel_height: u32,
    /// Laid-out document height in CSS pixels (before `max_height`).
    pub content_height: f32,
    /// Premultiplied RGBA8 pixels, row-major.
    pub rgba: Vec<u8>,
    /// Resources the provider served.
    pub served: usize,
    /// Resources the provider refused, in request order.
    pub refused: Vec<Refusal>,
    /// Time per phase.
    pub timings: Timings,
    scale: f32,
}

impl RenderedMail {
    /// Returns the `href` of the link under a point given in image (device)
    /// pixels, resolved against the message base, or `None`.
    ///
    /// The reader would route this through the "show real target URL" flow
    /// (#118) rather than opening it directly.
    pub fn link_at(&self, x: f32, y: f32) -> Option<String> {
        let hit = self.document.hit(x / self.scale, y / self.scale)?;
        let mut node_id = Some(hit.node_id);
        while let Some(id) = node_id {
            let node = self.document.get_node(id)?;
            if let Some(element) = node.element_data()
                && element.name.local == local_name!("a")
                && let Some(href) = element.attr(local_name!("href"))
            {
                return Some(
                    self.document
                        .base_url()
                        .join(href)
                        .map(|url| url.to_string())
                        .unwrap_or_else(|_| href.to_owned()),
                );
            }
            node_id = node.parent;
        }
        None
    }
}

/// Renders `html` (the sanitiser's output, never raw mail) to pixels.
///
/// Every resource the document references is requested from `resources`;
/// the engine performs no other loads. Panics inside the engine are caught
/// and returned as errors, but see the spike report for why in-process
/// `catch_unwind` is not a sufficient isolation boundary.
pub fn render(
    html: &str,
    resources: MailResources,
    options: RenderOptions,
) -> Result<RenderedMail> {
    catch_unwind(AssertUnwindSafe(|| render_inner(html, resources, options))).map_err(|panic| {
        let message = panic
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_owned());
        anyhow!("HTML renderer panicked: {message}")
    })?
}

fn render_inner(
    html: &str,
    resources: MailResources,
    options: RenderOptions,
) -> Result<RenderedMail> {
    let scale = options.scale.clamp(0.5, 4.0);
    let width = options.width.clamp(200, 4096);
    let pixel_width = (width as f32 * scale).round() as u32;
    let color_scheme = if options.dark {
        ColorScheme::Dark
    } else {
        ColorScheme::Light
    };
    // An initial viewport one "screen" tall; vh units resolve against it.
    let viewport = |css_height: u32| {
        Viewport::new(
            pixel_width,
            (css_height as f32 * scale).round() as u32,
            scale,
            color_scheme,
        )
    };

    let net = Arc::new(provider::MailNetProvider::new(resources));
    let started = Instant::now();
    let mut document = BaseDocument::new(DocumentConfig {
        base_url: Some(MAIL_BASE_URL.to_owned()),
        net_provider: Some(Arc::clone(&net) as _),
        // Measure with a short viewport so `height: 100%` documents report
        // their content height rather than the viewport's.
        viewport: Some(viewport(MEASURE_HEIGHT)),
        // Many readers may render at once; the parallel traversal shares
        // Stylo's global pool and panics under concurrent use.
        style_threading: StyleThreading::Sequential,
        ..Default::default()
    });
    parse_html(&mut document, html)?;
    let parse = started.elapsed();

    let started = Instant::now();
    // The provider answers synchronously, so one pass drains every
    // resource event; the second resolve lays out with them applied.
    document.resolve(0.0);
    document.resolve(0.0);
    let root = document.root_element().final_layout();
    // The root's padding box plus whatever its content overflows by.
    let content_height = root
        .size
        .height
        .max(root.padding.top + root.scrollable_overflow_rect.bottom);
    let css_height = (content_height.ceil() as u32).clamp(1, options.max_height);
    // Hit-testing and painting cover the whole document, not one screen.
    document.set_viewport(viewport(css_height));
    document.resolve(0.0);
    let layout = started.elapsed();

    let started = Instant::now();
    let pixel_height = (css_height as f32 * scale).round() as u32;
    let rgba = render_to_buffer::<VelloCpuImageRenderer, _>(
        |scene| {
            scene.fill(
                Fill::NonZero,
                Default::default(),
                if options.dark {
                    Color::from_rgb8(0x1e, 0x1e, 0x1e)
                } else {
                    Color::WHITE
                },
                Default::default(),
                &Rect::new(0.0, 0.0, pixel_width as f64, pixel_height as f64),
            );
            blitz_paint::paint_scene(
                scene,
                &mut document,
                scale as f64,
                pixel_width,
                pixel_height,
                0,
                0,
            );
        },
        pixel_width,
        pixel_height,
    );
    let raster = started.elapsed();

    let log = net.take_log();
    Ok(RenderedMail {
        document,
        pixel_width,
        pixel_height,
        content_height,
        rgba,
        served: log.served,
        refused: log.refused,
        timings: Timings {
            parse,
            layout,
            raster,
        },
        scale,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> RenderOptions {
        RenderOptions {
            width: 400,
            ..RenderOptions::default()
        }
    }

    #[test]
    fn renders_a_table_layout_and_finds_its_link() {
        let html = r#"<html><body style="margin:0">
            <table width="100%" cellpadding="0" cellspacing="0"><tr>
              <td style="height:40px;background:#0a7">header</td></tr>
              <tr><td style="padding:10px"><a href="https://example.org/receipt"
                style="display:block;height:30px">View receipt</a></td></tr>
            </table></body></html>"#;
        let rendered = render(html, MailResources::default(), options()).expect("renders");
        assert_eq!(rendered.pixel_width, 400);
        assert!(rendered.content_height >= 70.0);
        // The header cell paints its background colour (sampled right of
        // its text, so font metrics on each OS do not matter).
        let pixel = &rendered.rgba[(20 * 400 + 390) * 4..][..4];
        assert_eq!(pixel, &[0x00, 0xaa, 0x77, 0xff]);
        assert_eq!(
            rendered.link_at(50.0, 60.0).as_deref(),
            Some("https://example.org/receipt")
        );
        assert_eq!(rendered.link_at(50.0, 10.0), None);
    }

    #[test]
    fn remote_and_file_loads_never_leave_the_provider() {
        let html = r#"<html><head>
            <link rel="stylesheet" href="https://cdn.example/style.css">
            <style>@import url("file:///etc/passwd");
              body { background-image: url("http://tracker.example/bg.png"); }</style>
            </head><body><img src="https://tracker.example/open.gif" width="1" height="1">
            <img src="cid:missing@example"><img src="relative.png"></body></html>"#;
        let rendered = render(html, MailResources::default(), options()).expect("renders");
        assert_eq!(rendered.served, 0);
        let remote = rendered
            .refused
            .iter()
            .filter(|r| matches!(r, Refusal::Remote(_)))
            .count();
        assert!(remote >= 2, "refused: {:?}", rendered.refused);
        assert!(rendered.refused.contains(&Refusal::Scheme("file".into())));
        assert!(
            rendered
                .refused
                .contains(&Refusal::MissingCid("missing@example".into()))
        );
        assert!(
            rendered
                .refused
                .contains(&Refusal::Scheme("zmail-message".into()))
        );
    }

    #[test]
    fn svg_cannot_pull_local_files_into_the_render() {
        // usvg's default resolver would read `<image href="/abs/path">`
        // from disk; the build leaves Blitz's `svg` feature off.
        let path = std::env::temp_dir().join(format!(
            "zmail-html-render-svg-probe-{}.png",
            std::process::id()
        ));
        image::RgbImage::from_pixel(8, 8, image::Rgb([255, 0, 0]))
            .save(&path)
            .expect("probe image writes");
        let svg = format!(
            "<svg xmlns='http://www.w3.org/2000/svg' width='50' height='50'>\
             <image href='{}' width='50' height='50'/></svg>",
            path.display()
        );
        let html = format!(
            "<html><body style='margin:0'>{svg}\
             <img src=\"data:image/svg+xml,{}\" width='50' height='50'></body></html>",
            svg.replace('#', "%23")
        );
        let rendered = render(&html, MailResources::default(), options()).expect("renders");
        let _ = std::fs::remove_file(&path);
        let red = rendered
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .any(|[r, g, b, _]| *r > 200 && *g < 50 && *b < 50);
        assert!(!red, "a local file was painted into the message");
    }

    #[test]
    fn dark_scheme_answers_prefers_color_scheme() {
        let html = r#"<html><head><style>
            body { margin:0; background:#ffffff; }
            @media (prefers-color-scheme: dark) { body { background:#000000; } }
            </style></head><body><div style="height:20px"></div></body></html>"#;
        let light = render(html, MailResources::default(), options()).expect("renders");
        let dark = render(
            html,
            MailResources::default(),
            RenderOptions {
                dark: true,
                ..options()
            },
        )
        .expect("renders");
        assert_eq!(&light.rgba[..4], &[0xff, 0xff, 0xff, 0xff]);
        assert_eq!(&dark.rgba[..4], &[0x00, 0x00, 0x00, 0xff]);
    }
}
