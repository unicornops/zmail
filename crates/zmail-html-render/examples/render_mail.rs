//! Renders one `.eml` or `.html` file to a PNG with the spike renderer.
//!
//! ```text
//! cargo run --release -p zmail-html-render --example render_mail -- \
//!     INPUT OUTPUT.png [--width 800] [--scale 1] [--dark] \
//!     [--sanitizer current|keep-styles] [--link X,Y]
//! ```
//!
//! `--sanitizer current` (the default) feeds the engine exactly what the
//! reader stores today, `zmail_security::sanitize_html_for_display`, which
//! drops `<style>` blocks and rewrites `http(s)://` to `blocked://`.
//! `keep-styles` only removes `<script>` blocks; it stands in for the
//! style-preserving sanitiser faithful rendering would need (see the report).
//!
//! Prints one JSON line of measurements; `peak_rss_kib` is the process
//! high-water mark from `/proc/self/status` (Linux only, 0 elsewhere).

use std::{fs, path::Path};

use anyhow::{Context, Result, bail};
use mail_parser::{MessageParser, MimeHeaders, PartType};
use zmail_html_render::{InlinePart, MailResources, Refusal, RenderOptions, render};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let input = args.next().context("missing INPUT")?;
    let output = args.next().context("missing OUTPUT.png")?;
    let mut options = RenderOptions::default();
    let mut sanitizer = "current".to_owned();
    let mut link_probe = None;
    while let Some(flag) = args.next() {
        let mut value = || args.next().with_context(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--width" => options.width = value()?.parse()?,
            "--scale" => options.scale = value()?.parse()?,
            "--dark" => options.dark = true,
            "--sanitizer" => sanitizer = value()?,
            "--link" => {
                let raw = value()?;
                let (x, y) = raw.split_once(',').context("--link takes X,Y")?;
                link_probe = Some((x.parse::<f32>()?, y.parse::<f32>()?));
            }
            other => bail!("unknown flag {other}"),
        }
    }

    let raw = fs::read(&input).with_context(|| format!("reading {input}"))?;
    let (html, resources) = if Path::new(&input)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("eml"))
    {
        load_eml(&raw)?
    } else {
        (
            String::from_utf8_lossy(&raw).into_owned(),
            MailResources::default(),
        )
    };

    let html = match sanitizer.as_str() {
        "current" => zmail_security::sanitize_html_for_display(&html).html,
        "keep-styles" => strip_scripts(&html),
        other => bail!("unknown sanitizer {other}"),
    };

    let rendered = render(&html, resources, options)?;
    write_png(
        Path::new(&output),
        &rendered.rgba,
        rendered.pixel_width,
        rendered.pixel_height,
    )?;

    let remote = rendered
        .refused
        .iter()
        .filter(|r| matches!(r, Refusal::Remote(_)))
        .count();
    let link = link_probe
        .and_then(|(x, y)| rendered.link_at(x, y))
        .unwrap_or_default();
    println!(
        "{{\"input\":{input:?},\"sanitizer\":{sanitizer:?},\"width\":{},\"height\":{},\
         \"content_height\":{:.0},\"parse_ms\":{:.2},\"layout_ms\":{:.2},\"raster_ms\":{:.2},\
         \"total_ms\":{:.2},\"served\":{},\"refused\":{},\"refused_remote\":{remote},\
         \"peak_rss_kib\":{},\"link\":{link:?}}}",
        rendered.pixel_width,
        rendered.pixel_height,
        rendered.content_height,
        ms(rendered.timings.parse),
        ms(rendered.timings.layout),
        ms(rendered.timings.raster),
        ms(rendered.timings.total()),
        rendered.served,
        rendered.refused.len(),
        peak_rss_kib(),
    );
    Ok(())
}

fn ms(duration: std::time::Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// Picks the HTML body and collects every part with a `Content-ID`.
fn load_eml(raw: &[u8]) -> Result<(String, MailResources)> {
    let message = MessageParser::default()
        .parse(raw)
        .context("not a MIME message")?;
    let html = message
        .body_html(0)
        .context("message has no HTML body")?
        .into_owned();
    let mut resources = MailResources::default();
    for part in &message.parts {
        let (Some(cid), PartType::Binary(bytes) | PartType::InlineBinary(bytes)) =
            (part.content_id(), &part.body)
        else {
            continue;
        };
        let content_type = part
            .content_type()
            .map(|ct| match ct.subtype() {
                Some(sub) => format!("{}/{}", ct.ctype(), sub),
                None => ct.ctype().to_owned(),
            })
            .unwrap_or_default();
        resources.insert_inline(
            cid,
            InlinePart {
                content_type,
                bytes: bytes.to_vec().into(),
            },
        );
    }
    Ok((html, resources))
}

fn strip_scripts(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut cursor = 0;
    while let Some(start) = lower[cursor..].find("<script").map(|i| cursor + i) {
        out.push_str(&html[cursor..start]);
        cursor = lower[start..]
            .find("</script>")
            .map_or(html.len(), |end| start + end + "</script>".len());
    }
    out.push_str(&html[cursor..]);
    out
}

fn write_png(path: &Path, rgba: &[u8], width: u32, height: u32) -> Result<()> {
    let file = fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(rgba)?;
    writer.finish()?;
    Ok(())
}

fn peak_rss_kib() -> u64 {
    fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("VmHWM:"))
                .and_then(|rest| rest.trim().trim_end_matches("kB").trim().parse().ok())
        })
        .unwrap_or(0)
}
