//! Renders every `.html` file in a directory several times in one process
//! and prints per-file median timings, as a long-lived reader (or renderer
//! subprocess) would see them after the first message.
//!
//! ```text
//! cargo run --release -p zmail-html-render --example bench_corpus -- DIR [RUNS] [WIDTH] [SCALE]
//! ```
//!
//! The first render in a process also pays one-time setup (system font
//! enumeration, Stylo's global tables); it is reported separately.

use std::{fs, time::Instant};

use anyhow::{Context, Result};
use zmail_html_render::{MailResources, RenderOptions, render};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = args.next().context("missing DIR")?;
    let runs: usize = args.next().map(|r| r.parse()).transpose()?.unwrap_or(5);
    let width: u32 = args.next().map(|w| w.parse()).transpose()?.unwrap_or(600);
    let scale: f32 = args.next().map(|s| s.parse()).transpose()?.unwrap_or(1.0);
    let options = RenderOptions {
        width,
        scale,
        ..RenderOptions::default()
    };

    let mut files: Vec<_> = fs::read_dir(&dir)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "html"))
        .collect();
    files.sort();

    let started = Instant::now();
    render("<p>warm-up</p>", MailResources::default(), options)?;
    println!(
        "{{\"first_render_ms\":{:.2},\"rss_after_first_kib\":{}}}",
        started.elapsed().as_secs_f64() * 1000.0,
        rss_kib()
    );

    for path in &files {
        let html = fs::read_to_string(path)?;
        let mut totals = Vec::with_capacity(runs);
        for _ in 0..runs {
            let rendered = render(&html, MailResources::default(), options)?;
            totals.push(rendered.timings.total().as_secs_f64() * 1000.0);
        }
        totals.sort_by(f64::total_cmp);
        println!(
            "{{\"file\":{:?},\"median_ms\":{:.2},\"min_ms\":{:.2},\"rss_kib\":{}}}",
            path.file_name().unwrap_or_default(),
            totals[totals.len() / 2],
            totals[0],
            rss_kib()
        );
    }
    Ok(())
}

fn rss_kib() -> u64 {
    fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("VmRSS:"))
                .and_then(|rest| rest.trim().trim_end_matches("kB").trim().parse().ok())
        })
        .unwrap_or(0)
}
