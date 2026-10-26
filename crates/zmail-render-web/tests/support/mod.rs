//! What the canary tests share: the hostile corpus and the server that
//! counts every request made to it. (`canary.rs` still has its own copy
//! for macOS; the Linux test, `canary_linux.rs`, uses this one.)

#![allow(dead_code)]

use std::{
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use anyhow::{Context, Result};

/// A 1x1 transparent PNG.
pub const PIXEL_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x60, 0x00, 0x02, 0x00,
    0x00, 0x05, 0x00, 0x01, 0xe9, 0xfa, 0xdc, 0xd8, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44,
    0xae, 0x42, 0x60, 0x82,
];

/// A server that records every request it gets.
pub struct Canary {
    pub port: u16,
    pub hits: Arc<Mutex<Vec<String>>>,
}

impl Canary {
    pub fn start() -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").context("binding the canary")?;
        let port = listener.local_addr()?.port();
        let hits = Arc::new(Mutex::new(Vec::new()));
        let recorded = hits.clone();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let line = read_request_line(&stream);
                if let Ok(mut hits) = recorded.lock() {
                    hits.push(line);
                }
                let mut stream = stream;
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 0\r\n\r\n",
                );
            }
        });
        Ok(Self { port, hits })
    }

    /// Forgets the requests recorded so far.
    pub fn clear(&self) {
        if let Ok(mut hits) = self.hits.lock() {
            hits.clear();
        }
    }

    pub fn hits(&self) -> Vec<String> {
        self.hits
            .lock()
            .map(|hits| hits.clone())
            .unwrap_or_default()
    }
}

fn read_request_line(stream: &TcpStream) -> String {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let mut line = String::new();
    let _ = BufReader::new(stream).read_line(&mut line);
    let line = line.trim().to_owned();
    if line.is_empty() {
        "(a connection, no request line)".to_owned()
    } else {
        line
    }
}

/// Hostile mail: each entry references the canary (`HOST` is replaced
/// with `127.0.0.1:<port>`, `LOCAL` with `localhost:<port>`).
pub fn hostile_corpus() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "img",
            r#"<img src="http://HOST/img"><img src="https://HOST/img-tls"><img src="//HOST/img-scheme-relative"><img src="http://LOCAL/img-localhost">"#,
        ),
        (
            "srcset and picture",
            r#"<img srcset="http://HOST/srcset 1x, http://HOST/srcset2 2x" src="cid:none"><picture><source srcset="http://HOST/source"><img src="http://HOST/picture"></picture>"#,
        ),
        (
            "lazy and sized images",
            r#"<img loading="lazy" src="http://HOST/lazy"><img width="1" height="1" src="http://HOST/pixel" style="display:none"><input type="image" src="http://HOST/input-image">"#,
        ),
        (
            "background attributes",
            r#"<table background="http://HOST/table-bg"><tr><td background="http://HOST/td-bg">x</td></tr></table><body background="http://HOST/body-bg">"#,
        ),
        (
            "inline style urls",
            r#"<div style="background:url(http://HOST/inline-bg)">a</div><div style="background-image:image-set('http://HOST/image-set' 1x)">b</div><div style="cursor:url(http://HOST/cursor),auto">c</div><ul style="list-style-image:url(http://HOST/list)"><li>d</li></ul><div style="border:10px solid;border-image:url(http://HOST/border) 30">e</div><div style="-webkit-mask-image:url(http://HOST/mask)">f</div>"#,
        ),
        (
            "style block",
            r#"<style>@import url(http://HOST/import.css); @font-face{font-family:x;src:url(http://HOST/font.woff)} body{background:url(http://HOST/style-bg)} p::after{content:url(http://HOST/content)} p{font-family:x}</style><p>text</p>"#,
        ),
        (
            "link elements",
            r#"<link rel="stylesheet" href="http://HOST/link.css"><link rel="prefetch" href="http://HOST/prefetch"><link rel="preload" as="image" href="http://HOST/preload"><link rel="dns-prefetch" href="http://HOST/"><link rel="preconnect" href="http://HOST/"><link rel="icon" href="http://HOST/favicon.ico">"#,
        ),
        (
            "scripts",
            r#"<script src="http://HOST/script.js"></script><script>fetch('http://HOST/fetch');new Image().src='http://HOST/js-image';</script><img src="cid:missing" onerror="this.src='http://HOST/onerror'"><body onload="location='http://HOST/onload'">"#,
        ),
        (
            "frames and objects",
            r#"<iframe src="http://HOST/iframe"></iframe><iframe srcdoc="<img src='http://HOST/srcdoc'>"></iframe><object data="http://HOST/object"></object><embed src="http://HOST/embed"><frameset><frame src="http://HOST/frame"></frameset>"#,
        ),
        (
            "media",
            r#"<video src="http://HOST/video" poster="http://HOST/poster" autoplay></video><audio src="http://HOST/audio" autoplay></audio><video><source src="http://HOST/source-video"><track src="http://HOST/track"></video>"#,
        ),
        (
            "svg",
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="10" height="10"><image href="http://HOST/svg-image" width="10" height="10"/><image xlink:href="http://HOST/svg-xlink" width="10" height="10"/><use href="http://HOST/svg-use#x"/><feImage href="http://HOST/feimage"/></svg><img src="data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg'%3E%3Cimage href='http://HOST/svg-in-data'/%3E%3C/svg%3E">"#,
        ),
        (
            "navigation",
            r#"<meta http-equiv="refresh" content="0;url=http://HOST/refresh"><a href="http://HOST/ping-target" ping="http://HOST/ping">link</a><form action="http://HOST/form"><input name="q" autofocus></form><base href="http://HOST/base/"><img src="relative.png">"#,
        ),
        (
            "local files",
            r#"<img src="file:///etc/hosts"><iframe src="file:///etc/passwd"></iframe><link rel="stylesheet" href="file:///System/Library/x.css"><img src="http://HOST/after-file">"#,
        ),
        (
            "whole document",
            r##"<!DOCTYPE html><html><head><meta http-equiv="Content-Security-Policy" content="default-src *"><style>body{background:url(http://HOST/doc-bg)}</style></head><body bgcolor="#ffffff"><img src="http://HOST/doc-img"></body></html>"##,
        ),
    ]
}
