//! The zero-network canary (ADR 0006) and checks of the Linux web view's
//! hardening, run against real WebKitGTK web views.
//!
//! A local HTTP server counts every connection. A corpus of hostile mail
//! references it in every way HTML and CSS can load something; each page
//! renders in the same hardened, sandboxed web view the reader uses, and
//! neither the server nor the engine's black-hole proxy may see anything.
//! The other checks: the web processes run in their own namespaces (the
//! sandbox started), page JavaScript does not run, `cid:` parts load
//! through Zmail's scheme handler, and the page is measured (with page
//! JavaScript off) and zoomed to fit its width.
//!
//! It needs a display (Xvfb, or a headless Wayland compositor) and
//! WebKitGTK 4.1; without a display it has nothing to check. CI's
//! "Web renderer (Linux)" job sets `ZMAIL_CANARY_EXPECT`:
//!
//! - `running`: the engine must start and every check must pass;
//! - `refused`: the engine must refuse to start because its sandbox
//!   cannot (Ubuntu 24.04 without an AppArmor profile), without crashing.
//!
//! Unset, a refusal is reported and the test passes (a developer machine
//! or container without user namespaces).

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("canary_linux: Linux only; nothing to check here");
}

#[cfg(target_os = "linux")]
mod support;

#[cfg(target_os = "linux")]
fn main() {
    match linux::run() {
        Ok(()) => println!("canary_linux: all checks passed"),
        Err(error) => {
            eprintln!("canary_linux: FAILED: {error:#}");
            std::process::exit(1);
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::{
        io::Write,
        net::TcpStream,
        sync::Arc,
        thread,
        time::{Duration, Instant},
    };

    use anyhow::{Context, Result, bail, ensure};
    use futures_channel::mpsc::{UnboundedReceiver, unbounded};
    use zmail_render::{PrepareOptions, RemoteContentDecision, RenderSource, prepare};
    use zmail_render_web::{
        Appearance, InlineResource, WebDocument, WebEvent,
        linux_testing::{
            DisplayServer, PlatformWebView, ViewEvent, ViewSize, blocked_connections, failure,
            shutdown, start, verified_web_processes,
        },
    };

    use crate::support::{Canary, PIXEL_PNG, hostile_corpus};

    /// How long a page may take to load and be measured.
    const LOAD_TIMEOUT: Duration = Duration::from_secs(30);

    /// How long to keep listening after a page is measured, for loads that
    /// start late (lazy images, refreshes).
    const SETTLE: Duration = Duration::from_millis(1500);

    const SIZE: ViewSize = ViewSize {
        width: 600.,
        height: 200.,
        scale: 1.,
    };

    /// A render in progress, and what it reported.
    struct Rendered {
        view: PlatformWebView,
        receiver: UnboundedReceiver<ViewEvent>,
        events: Vec<ViewEvent>,
    }

    impl Rendered {
        /// Collects the events that arrived.
        fn poll(&mut self) {
            while let Ok(event) = self.receiver.try_recv() {
                self.events.push(event);
            }
        }

        fn measured_height(&self) -> Option<f32> {
            self.events.iter().rev().find_map(|event| match event {
                ViewEvent::Web(WebEvent::Measured { height }) => Some(*height),
                _ => None,
            })
        }

        fn failure(&self) -> Option<String> {
            self.events.iter().find_map(|event| match event {
                ViewEvent::Web(WebEvent::LoadFailed(reason)) => Some(reason.clone()),
                ViewEvent::Web(WebEvent::ContentProcessTerminated) => {
                    Some("content process ended".to_owned())
                }
                _ => None,
            })
        }

        /// Runs `script` in Zmail's isolated world and waits for its result.
        fn evaluate(&mut self, script: &str) -> Result<String> {
            static NEXT_TAG: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
            let tag = NEXT_TAG.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.view.evaluate(script, tag);
            let started = Instant::now();
            loop {
                self.poll();
                let result = self.events.iter().find_map(|event| match event {
                    ViewEvent::Script { tag: done, result } if *done == tag => Some(result.clone()),
                    _ => None,
                });
                if let Some(result) = result {
                    return result.map_err(|error| anyhow::anyhow!("script failed: {error}"));
                }
                ensure!(
                    started.elapsed() < LOAD_TIMEOUT,
                    "the script did not finish"
                );
                thread::sleep(Duration::from_millis(20));
            }
        }
    }

    /// Renders `document` in a 600-pixel-wide view and waits until it is
    /// measured (or fails), then lets it settle.
    fn render(document: &WebDocument) -> Result<Rendered> {
        let (sender, receiver) = unbounded();
        let view = PlatformWebView::new(document, SIZE, sender)
            .map_err(|reason| anyhow::anyhow!("no web view: {reason}"))?;
        let mut rendered = Rendered {
            view,
            receiver,
            events: Vec::new(),
        };
        let started = Instant::now();
        loop {
            rendered.poll();
            if rendered.measured_height().is_some() || rendered.failure().is_some() {
                break;
            }
            if let Some(reason) = failure() {
                bail!("the engine stopped: {reason}");
            }
            ensure!(
                started.elapsed() < LOAD_TIMEOUT,
                "the page was not measured within {LOAD_TIMEOUT:?}"
            );
            thread::sleep(Duration::from_millis(20));
        }
        thread::sleep(SETTLE);
        rendered.poll();
        Ok(rendered)
    }

    fn document(html: &str) -> WebDocument {
        WebDocument::new(html, Appearance::default())
    }

    fn display() -> Option<DisplayServer> {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            Some(DisplayServer::Wayland)
        } else if std::env::var_os("DISPLAY").is_some() {
            Some(DisplayServer::X11)
        } else {
            None
        }
    }

    pub(super) fn run() -> Result<()> {
        let expect = std::env::var("ZMAIL_CANARY_EXPECT").unwrap_or_default();
        let Some(display) = display() else {
            ensure!(
                expect.is_empty(),
                "ZMAIL_CANARY_EXPECT={expect} but there is no display"
            );
            println!("canary_linux: no display; nothing to check here");
            return Ok(());
        };
        println!("canary_linux: display {display:?}");
        match (start(display), expect.as_str()) {
            (Ok(()), "refused") => bail!("the engine started, but it was expected to refuse"),
            (Ok(()), _) => {}
            (Err(reason), "refused") => {
                println!("canary_linux: refused as expected: {reason}");
                ensure!(
                    reason.contains("sandbox"),
                    "the refusal does not name the sandbox: {reason}"
                );
                return Ok(());
            }
            (Err(reason), "running") => bail!("the engine did not start: {reason}"),
            (Err(reason), _) => {
                println!("canary_linux: the engine is unavailable here: {reason}");
                return Ok(());
            }
        }

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
            canary.clear();
        }

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
                Err(reason) => println!("canary_linux: {name}: no prepared HTML ({reason})"),
            }
            for (form, page) in documents {
                let rendered = render(&page)
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
                    canary.clear();
                }
                println!(
                    "canary_linux: {name} ({form}): measured {:?}, {} requests",
                    rendered.measured_height(),
                    hits.len()
                );
            }
        }

        // Remote content allowed: images come through Zmail's fetcher, which
        // refuses any host that resolves to a private address. These public
        // names resolve to 127.0.0.1 (the canary), as a rebinding attack
        // would, so they must still never connect.
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
        let rendered = render(&direct)?;
        thread::sleep(Duration::from_secs(3));
        let hits = canary.hits();
        if !hits.is_empty() {
            problems.push(format!(
                "allowed remote content reached a private address: {hits:?}"
            ));
        }
        drop(rendered);

        // Page JavaScript does not run; Zmail's isolated world does.
        let mut rendered = render(&document(
            r#"<title>safe</title><script>document.title='script ran'</script><img src="cid:missing" onerror="document.title='handler ran'"><p>text</p>"#,
        ))?;
        let title = rendered.evaluate("document.title")?;
        println!("canary_linux: title after scripts: {title:?}");
        if title != "safe" {
            problems.push(format!("page JavaScript ran: the title is {title:?}"));
        }
        drop(rendered);

        // A cid: part loads through the scheme handler; an unknown one
        // does not. Three times, as the first CI runs saw it fail once in a
        // while.
        for attempt in 1..=3 {
            let mut with_part = document(
                r#"<img id="known" src="cid:logo@example.com"><img id="unknown" src="cid:other@example.com">"#,
            );
            with_part.resources.push(InlineResource {
                content_id: "<Logo@Example.com>".to_owned(),
                mime_type: "image/png".to_owned(),
                data: Arc::from(PIXEL_PNG),
            });
            let mut rendered = render(&with_part)?;
            // WebKit may still be decoding the image when the page is
            // measured.
            let started = Instant::now();
            let widths = loop {
                let widths = rendered.evaluate(
                    "document.getElementById('known').naturalWidth + ' ' + document.getElementById('unknown').naturalWidth",
                )?;
                if widths == "1 0" || started.elapsed() > Duration::from_secs(5) {
                    break widths;
                }
                thread::sleep(Duration::from_millis(200));
            };
            println!(
                "canary_linux: cid image widths (known, unknown), attempt {attempt}: {widths}"
            );
            if widths != "1 0" {
                let state = rendered.evaluate(
                    "(() => { const k = document.getElementById('known'); \
                     return [k.complete, k.naturalWidth, k.currentSrc, \
                     performance.getEntriesByType('resource').map(e => e.name).join(',')].join(' | '); })()",
                )?;
                println!("canary_linux: the known cid image: {state}");
                problems.push(format!(
                    "cid: parts did not resolve as expected (attempt {attempt}): {widths} ({state})"
                ));
            }
            drop(rendered);
        }

        // The page is measured with page JavaScript off.
        let rendered = render(&document(
            r#"<div style="height:1234px;background:#eee">tall</div>"#,
        ))?;
        match rendered.measured_height() {
            Some(height) if (1234.0..1300.0).contains(&height) => {
                println!("canary_linux: a 1234px page measured {height}")
            }
            other => problems.push(format!("a 1234px page measured {other:?}")),
        }
        drop(rendered);

        // A page wider than the view is zoomed out to fit.
        let rendered = render(&document(
            r#"<div style="width:1200px;height:1000px;background:#eee">wide</div>"#,
        ))?;
        match rendered.measured_height() {
            Some(height) if height < 700. => {
                println!("canary_linux: a 1200px-wide page fitted, height {height}")
            }
            other => problems.push(format!("a 1200px-wide page was not fitted: {other:?}")),
        }
        drop(rendered);

        // The walls behind the filter saw nothing, and the sandbox held.
        let blocked = blocked_connections();
        if blocked != 0 {
            problems.push(format!(
                "{blocked} connection(s) got past the content filter to the proxy wall"
            ));
        }
        let sandboxed = verified_web_processes();
        println!("canary_linux: {sandboxed} web process(es) confirmed sandboxed");
        if sandboxed == 0 {
            problems.push("no web process was confirmed sandboxed".to_owned());
        }
        if let Some(reason) = failure() {
            problems.push(format!("the engine stopped: {reason}"));
        }

        // Stop the engine before the process exits, as the app does.
        shutdown();

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
}
