//! The zero-network canary (ADR 0006, Phase 1 prototype 4) and checks of
//! the web view's hardening, run against real `WKWebView`s on macOS and
//! real WebView2 views on Windows.
//!
//! A local HTTP server counts every connection. A corpus of hostile mail
//! references it in every way HTML and CSS can load something; each page
//! renders in the same hardened web view the reader uses, and the server
//! must see nothing. The other checks: page JavaScript does not run, `cid:`
//! parts load through Zmail's scheme handler, and the page is measured
//! (with page JavaScript off) and zoomed to fit its width.
//!
//! On Windows it also checks Select All and Copy (which go through Zmail's
//! scripts and the clipboard there), and that DirectComposition refuses to
//! re-add a child visual, the reason the host works around GPUI's tree
//! rebuild.
//!
//! Web views must run on the main thread, so this test has its own `main`
//! (`harness = false`). It only does anything on macOS and Windows; CI runs
//! it on both runners.

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn main() {
    println!("canary: the web renderer exists only on macOS and Windows; nothing to check here");
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn main() {
    match canary::run() {
        Ok(()) => println!("canary: all checks passed"),
        Err(error) => {
            eprintln!("canary: FAILED: {error:#}");
            std::process::exit(1);
        }
    }
}

/// The platform's web views behind one small API.
#[cfg(target_os = "macos")]
mod engine {
    use std::{rc::Rc, time::Duration};

    use anyhow::Result;
    use zmail_render_web::{
        WebDocument, WebEvent,
        macos_testing::{Frame, OffscreenWindow},
    };

    pub(crate) use zmail_render_web::macos_testing::PlatformWebView as View;

    pub(crate) struct Window(OffscreenWindow);

    impl Window {
        pub(crate) fn new() -> Result<Self> {
            Ok(Self(OffscreenWindow::new(800., 600.)?))
        }

        /// A web view 600 points wide showing `document`.
        pub(crate) fn view(
            &self,
            document: &WebDocument,
            on_event: Rc<dyn Fn(WebEvent)>,
        ) -> Result<View> {
            let view = View::new(self.0.clip(), document, on_event)?;
            view.set_frame(Frame {
                x: 0.,
                y: 0.,
                width: 600.,
                height: 200.,
            });
            view.set_hidden(false);
            Ok(view)
        }

        pub(crate) fn pump(&self, duration: Duration) {
            self.0.pump(duration);
        }
    }
}

/// The platform's web views behind one small API.
#[cfg(target_os = "windows")]
mod engine {
    use std::{rc::Rc, time::Duration};

    use anyhow::Result;
    use zmail_render_web::{WebDocument, WebEvent, windows_testing::TestWindow};

    pub(crate) use zmail_render_web::windows_testing::PlatformWebView as View;

    pub(crate) struct Window(pub(crate) TestWindow);

    impl Window {
        pub(crate) fn new() -> Result<Self> {
            Ok(Self(TestWindow::new(800, 600)?))
        }

        /// A web view 600 pixels wide (at scale 1) showing `document`.
        pub(crate) fn view(
            &self,
            document: &WebDocument,
            on_event: Rc<dyn Fn(WebEvent)>,
        ) -> Result<View> {
            let view = View::new(&self.0.target()?, document, on_event)?;
            view.set_scale(1.);
            view.set_size(600, 200);
            view.set_hidden(false);
            Ok(view)
        }

        pub(crate) fn pump(&self, duration: Duration) {
            self.0.pump(duration);
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod canary {
    use std::{
        cell::RefCell,
        io::{BufRead, BufReader, Write},
        net::{TcpListener, TcpStream},
        rc::Rc,
        sync::{Arc, Mutex},
        thread,
        time::{Duration, Instant},
    };

    use anyhow::{Context, Result, bail, ensure};
    use zmail_render::{PrepareOptions, RemoteContentDecision, RenderSource, prepare};
    use zmail_render_web::{Appearance, InlineResource, WebDocument, WebEvent};

    use crate::engine::{View, Window};

    /// How long a page may take to load and be measured.
    const LOAD_TIMEOUT: Duration = Duration::from_secs(20);

    /// How long to keep listening after a page is measured, for loads that
    /// start late (lazy images, refreshes).
    const SETTLE: Duration = Duration::from_millis(1500);

    /// A 1x1 transparent PNG.
    const PIXEL_PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x60,
        0x00, 0x02, 0x00, 0x00, 0x05, 0x00, 0x01, 0xe9, 0xfa, 0xdc, 0xd8, 0x00, 0x00, 0x00, 0x00,
        0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    /// A server that records every request it gets.
    struct Canary {
        port: u16,
        hits: Arc<Mutex<Vec<String>>>,
    }

    impl Canary {
        fn start() -> Result<Self> {
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

        fn hits(&self) -> Vec<String> {
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
    fn hostile_corpus() -> Vec<(&'static str, &'static str)> {
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

    /// What a render produced.
    struct Rendered {
        view: View,
        events: Rc<RefCell<Vec<WebEvent>>>,
    }

    impl Rendered {
        fn measured_height(&self) -> Option<f32> {
            self.events
                .borrow()
                .iter()
                .rev()
                .find_map(|event| match event {
                    WebEvent::Measured { height } => Some(*height),
                    _ => None,
                })
        }

        fn failure(&self) -> Option<String> {
            self.events.borrow().iter().find_map(|event| match event {
                WebEvent::LoadFailed(reason) => Some(reason.clone()),
                WebEvent::ContentProcessTerminated => Some("content process ended".to_owned()),
                _ => None,
            })
        }
    }

    /// Renders `document` in a 600-point-wide view and waits until it is
    /// measured (or fails), then lets it settle.
    fn render(window: &Window, document: &WebDocument) -> Result<Rendered> {
        let events = Rc::new(RefCell::new(Vec::new()));
        let recorded = events.clone();
        let view = window.view(
            document,
            Rc::new(move |event| recorded.borrow_mut().push(event)),
        )?;
        let rendered = Rendered { view, events };
        let started = Instant::now();
        while rendered.measured_height().is_none() && rendered.failure().is_none() {
            if started.elapsed() > LOAD_TIMEOUT {
                bail!("the page was not measured within {LOAD_TIMEOUT:?}");
            }
            window.pump(Duration::from_millis(50));
        }
        window.pump(SETTLE);
        Ok(rendered)
    }

    /// Runs `script` in the isolated world and waits for its result.
    fn evaluate(window: &Window, rendered: &Rendered, script: &str) -> Result<String> {
        let result: Rc<RefCell<Option<Result<String, String>>>> = Rc::new(RefCell::new(None));
        let slot = result.clone();
        rendered
            .view
            .evaluate(script, move |value| *slot.borrow_mut() = Some(value));
        let started = Instant::now();
        loop {
            if let Some(value) = result.borrow_mut().take() {
                return value.map_err(|error| anyhow::anyhow!("script failed: {error}"));
            }
            if started.elapsed() > LOAD_TIMEOUT {
                bail!("the script did not finish");
            }
            window.pump(Duration::from_millis(20));
        }
    }

    fn document(html: &str) -> WebDocument {
        WebDocument::new(html, Appearance::default())
    }

    pub(super) fn run() -> Result<()> {
        let canary = Canary::start()?;
        // The canary itself works: a direct connection is recorded.
        {
            let mut stream = TcpStream::connect(("127.0.0.1", canary.port))?;
            stream.write_all(b"GET /self-test HTTP/1.1\r\nHost: canary\r\n\r\n")?;
            thread::sleep(Duration::from_millis(300));
            ensure!(
                canary.hits().iter().any(|hit| hit.contains("/self-test")),
                "the canary did not record its self-test"
            );
            canary.hits.lock().map(|mut hits| hits.clear()).ok();
        }

        let window = Window::new()?;
        let host = format!("127.0.0.1:{}", canary.port);
        let local = format!("localhost:{}", canary.port);
        let mut problems = Vec::new();

        // Each hostile page twice: raw, so the web view's own walls are
        // tested without the sanitiser, and as Zmail shows mail, prepared
        // by `zmail_render::prepare` (with remote content blocked).
        for (name, template) in hostile_corpus() {
            let html = template.replace("HOST", &host).replace("LOCAL", &local);
            let prepared = prepare(
                &RenderSource {
                    message_id: 1,
                    html: Some(html.clone()),
                    ..RenderSource::default()
                },
                &PrepareOptions::default(),
            );
            let mut documents = vec![("raw", document(&html))];
            match WebDocument::from_prepared(
                &prepared,
                Appearance::default(),
                RemoteContentDecision::Blocked,
            ) {
                Ok(prepared) => documents.push(("prepared", prepared)),
                Err(reason) => println!("canary: {name}: no prepared HTML ({reason})"),
            }
            for (form, page) in documents {
                let rendered = render(&window, &page)
                    .with_context(|| format!("rendering hostile page {name:?} ({form})"))?;
                if let Some(reason) = rendered.failure() {
                    problems.push(format!(
                        "hostile page {name:?} ({form}) did not render: {reason}"
                    ));
                }
                let hits = canary.hits();
                if !hits.is_empty() {
                    problems.push(format!(
                        "hostile page {name:?} ({form}) reached the network: {hits:?}"
                    ));
                    canary.hits.lock().map(|mut hits| hits.clear()).ok();
                }
                println!(
                    "canary: {name} ({form}): measured {:?}, {} requests",
                    rendered.measured_height(),
                    hits.len()
                );
            }
        }

        // Remote content allowed: images come through Zmail's fetcher, which
        // resolves each host and refuses any that resolves to a private
        // address. These public names resolve to 127.0.0.1 (the canary), as
        // a rebinding attack would, so they must still never connect.
        let rebinding = format!(
            r#"<img src="https://127.0.0.1.nip.io:{port}/rebind.png"><div style="background:url(https://localtest.me:{port}/rebind-bg.png)">x</div>"#,
            port = canary.port
        );
        let prepared = prepare(
            &RenderSource {
                message_id: 2,
                html: Some(rebinding),
                ..RenderSource::default()
            },
            &PrepareOptions::default(),
        );
        let allowed = WebDocument::from_prepared(
            &prepared,
            Appearance::default(),
            RemoteContentDecision::AllowedForMessage,
        )
        .map_err(|reason| anyhow::anyhow!("the rebinding page has no HTML: {reason}"))?;
        println!(
            "canary: rebinding page fetches {:?} after preparing",
            allowed.remote.iter().flatten().collect::<Vec<_>>()
        );
        let rendered = render(&window, &allowed)?;
        window.pump(Duration::from_secs(3));
        drop(rendered);
        // The sanitiser may already refuse those addresses; give Zmail's
        // fetcher the same ones directly, so its own resolve-time check is
        // what keeps the canary silent.
        let mut direct = document(
            r#"<img src="zmail-mail://remote/0"><div style="background:url(zmail-mail://remote/1)">x</div>"#,
        );
        direct.remote = vec![
            Some(format!(
                "https://127.0.0.1.nip.io:{}/rebind.png",
                canary.port
            )),
            Some(format!(
                "https://localtest.me:{}/rebind-bg.png",
                canary.port
            )),
        ];
        let rendered = render(&window, &direct)?;
        window.pump(Duration::from_secs(3));
        let hits = canary.hits();
        if !hits.is_empty() {
            problems.push(format!(
                "allowed remote content reached a private address: {hits:?}"
            ));
        }
        drop(rendered);

        // Page JavaScript does not run.
        let rendered = render(
            &window,
            &document(
                r#"<title>safe</title><script>document.title='script ran'</script><img src="cid:missing" onerror="document.title='handler ran'"><p>text</p>"#,
            ),
        )?;
        let title = evaluate(&window, &rendered, "document.title")?;
        println!("canary: title after scripts: {title:?}");
        if title != "safe" {
            problems.push(format!("page JavaScript ran: the title is {title:?}"));
        }

        // A cid: part loads through the scheme handler; an unknown one
        // does not.
        let mut with_part = document(
            r#"<img id="known" src="cid:logo@example.com"><img id="unknown" src="cid:other@example.com">"#,
        );
        with_part.resources.push(InlineResource {
            content_id: "<Logo@Example.com>".to_owned(),
            mime_type: "image/png".to_owned(),
            data: Arc::from(PIXEL_PNG),
        });
        let rendered = render(&window, &with_part)?;
        let widths = evaluate(
            &window,
            &rendered,
            "document.getElementById('known').naturalWidth + ' ' + document.getElementById('unknown').naturalWidth",
        )?;
        println!("canary: cid image widths (known, unknown): {widths}");
        if widths != "1 0" {
            problems.push(format!("cid: parts did not resolve as expected: {widths}"));
        }

        // The page is measured with page JavaScript off.
        let rendered = render(
            &window,
            &document(r#"<div style="height:1234px;background:#eee">tall</div>"#),
        )?;
        match rendered.measured_height() {
            Some(height) if (1234.0..1300.0).contains(&height) => {
                println!("canary: a 1234px page measured {height}")
            }
            other => problems.push(format!("a 1234px page measured {other:?}")),
        }

        // A page wider than the view is zoomed out to fit.
        let rendered = render(
            &window,
            &document(r#"<div style="width:1200px;height:1000px;background:#eee">wide</div>"#),
        )?;
        match rendered.measured_height() {
            Some(height) if height < 700. => {
                println!("canary: a 1200px-wide page fitted, height {height}")
            }
            other => problems.push(format!("a 1200px-wide page was not fitted: {other:?}")),
        }

        #[cfg(target_os = "windows")]
        windows_checks(&window, &mut problems)?;

        drop(window);
        if problems.is_empty() {
            Ok(())
        } else {
            bail!(
                "{} problem(s):\n  {}",
                problems.len(),
                problems.join("\n  ")
            )
        }
    }

    /// Select All and Copy go through Zmail's scripts and the clipboard on
    /// Windows; and DirectComposition refuses to re-add a child visual,
    /// which the host's GPUI workaround relies on.
    #[cfg(target_os = "windows")]
    fn windows_checks(window: &Window, problems: &mut Vec<String>) -> Result<()> {
        use zmail_render_web::windows_testing::clipboard_text;

        let rendered = render(
            window,
            &document("<p>Copy <b>this</b> text</p><p>and this</p>"),
        )?;
        rendered.view.select_all();
        window.pump(Duration::from_millis(500));
        rendered.view.copy();
        window.pump(Duration::from_millis(500));
        let copied = clipboard_text().unwrap_or_default();
        println!("canary: copied {copied:?}");
        if !(copied.contains("Copy this text") && copied.contains("and this")) {
            problems.push(format!(
                "Select All and Copy put {copied:?} on the clipboard"
            ));
        }
        match window.0.readding_a_child_fails() {
            Ok(true) => {}
            Ok(false) => println!(
                "canary: DirectComposition re-added a child visual; the host's detach is no longer needed"
            ),
            Err(error) => problems.push(format!("the DirectComposition check failed: {error:#}")),
        }
        Ok(())
    }
}
