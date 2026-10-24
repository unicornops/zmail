use std::sync::Arc;

use super::*;

struct Bytes(&'static [u8]);

impl PartBytes for Bytes {
    fn read(&self) -> anyhow::Result<Vec<u8>> {
        Ok(self.0.to_vec())
    }
}

fn newsletter() -> RenderSource {
    RenderSource {
        message_id: 7,
        body_text: None,
        html: Some(
            concat!(
                "<html><head><style>.hero { background: url(https://cdn.example.com/bg.png) }</style></head>",
                "<body><table><tr><td class=hero><img src=\"cid:logo@example.com\" alt=Logo></td>",
                "<td><p>Hello <a href=\"http://example.com/offer\">see the offer</a>.</p>",
                "<img src=\"https://cdn.example.com/hero.png\" alt=Hero>",
                "<img src=\"http://tracker.example.net/p.gif\" width=1 height=1></td></tr></table>",
                "</body></html>",
            )
            .to_owned(),
        ),
        html_is_legacy: false,
        inline_parts: vec![InlinePart::new(
            "logo@example.com",
            "image/png",
            4,
            Arc::new(Bytes(b"\x89PNG")),
        )],
    }
}

#[test]
fn blocked_remote_content_never_appears_in_the_html() {
    let prepared = prepare(&newsletter(), &PrepareOptions::default());
    let html = prepared.html().expect("the newsletter sanitises");
    assert!(!html.contains("cdn.example.com"), "{html}");
    assert!(!html.contains("tracker.example.net"), "{html}");
    assert!(html.contains("zmail-blocked:0"), "{html}");
    assert!(html.contains("src=\"cid:logo@example.com\""), "{html}");
    assert!(html.contains("href=\"zmail-link:0\""), "{html}");
    assert_eq!(prepared.remote.decision, RemoteContentDecision::Blocked);
    assert_eq!(
        prepared.remote.origins(),
        ["https://cdn.example.com", "http://tracker.example.net"]
    );
    assert!(prepared.remote.allowed_origins().is_empty());
    assert_eq!(prepared.remote.blocked_count(), 3);
    assert_eq!(prepared.theme, ThemeHint::Light);
}

#[test]
fn allowed_remote_content_loads_only_public_https() {
    let prepared = prepare(
        &newsletter(),
        &PrepareOptions {
            remote: RemoteContentDecision::AllowedForSender,
            theme: ThemeHint::Dark,
        },
    );
    let html = prepared.html().expect("the newsletter sanitises");
    assert!(html.contains("https://cdn.example.com/hero.png"), "{html}");
    assert!(
        html.contains("url(\"https://cdn.example.com/bg.png\")"),
        "{html}"
    );
    assert!(!html.contains("tracker.example.net"), "{html}");
    assert_eq!(
        prepared.remote.allowed_origins(),
        ["https://cdn.example.com"]
    );
    assert_eq!(prepared.remote.blocked_count(), 1);
    assert_eq!(prepared.theme, ThemeHint::Dark);
}

#[test]
fn links_inline_parts_and_text_come_with_the_html() {
    let prepared = prepare(&newsletter(), &PrepareOptions::default());
    assert_eq!(
        prepared
            .link_for_url("zmail-link:0")
            .map(|link| link.url.as_str()),
        Some("http://example.com/offer"),
        "the scheme is kept"
    );
    assert_eq!(
        prepared.link(0).map(|link| link.text.as_str()),
        Some("see the offer")
    );
    assert_eq!(prepared.link_for_url("zmail-link:9"), None);
    assert_eq!(prepared.link_for_url("https://example.com/"), None);
    assert_eq!(prepared.cid_references, ["logo@example.com"]);
    for name in [
        "cid:logo@example.com",
        "<logo@example.com>",
        "LOGO@example.com",
    ] {
        let part = prepared.inline_part(name).expect("the logo resolves");
        assert_eq!(part.content_type, "image/png");
        assert_eq!(part.read().expect("bytes"), b"\x89PNG");
    }
    assert_eq!(prepared.text, "Hello see the offer.");
    assert!(!prepared.text_is_plain);
    let link = &prepared.text_links[0];
    assert_eq!(&prepared.text[link.range.clone()], "see the offer");
    assert_eq!(link.link, 0);
}

#[test]
fn plain_text_wins_for_the_text_and_html_still_renders() {
    let mut source = newsletter();
    source.body_text = Some("Hello in plain text".to_owned());
    let prepared = prepare(&source, &PrepareOptions::default());
    assert_eq!(prepared.text, "Hello in plain text");
    assert!(prepared.text_is_plain);
    assert!(prepared.text_links.is_empty());
    assert!(prepared.html().is_ok());
    assert_eq!(
        prepared.link(0).map(|link| link.text.as_str()),
        Some("see the offer"),
        "links keep their text when the text view shows the plain part"
    );
    assert!(TextRenderer.accepts(&prepared));
}

#[test]
fn messages_without_usable_html_say_why() {
    let source = RenderSource {
        message_id: 1,
        body_text: Some("Only text".to_owned()),
        ..RenderSource::default()
    };
    let prepared = prepare(&source, &PrepareOptions::default());
    assert_eq!(prepared.html(), Err(HtmlUnavailable::NoHtml));
    assert_eq!(prepared.text, "Only text");

    struct Web;
    impl MessageRenderer for Web {
        fn kind(&self) -> RendererKind {
            RendererKind::Web
        }
    }
    assert!(!Web.accepts(&prepared));

    let deep = RenderSource {
        message_id: 2,
        html: Some(format!(
            "{}<a href=\"https://example.com/\">deep</a>{}",
            "<table><tr><td>".repeat(1_000),
            "</td></tr></table>".repeat(1_000)
        )),
        ..RenderSource::default()
    };
    let prepared = prepare(&deep, &PrepareOptions::default());
    assert_eq!(
        prepared.html(),
        Err(HtmlUnavailable::TooComplex(Complexity::TooDeep))
    );
    assert_eq!(prepared.text, "deep");
    assert_eq!(
        prepared.link(0).map(|link| link.url.as_str()),
        Some("https://example.com/")
    );
}

#[test]
fn legacy_bodies_keep_their_links_over_https() {
    let source = RenderSource {
        message_id: 3,
        html: Some(
            "<p>Read <a href=\"blocked://example.com/post\">the post</a>\
             <img src=\"blocked://example.com/p.gif\"></p>"
                .to_owned(),
        ),
        html_is_legacy: true,
        ..RenderSource::default()
    };
    let prepared = prepare(&source, &PrepareOptions::default());
    assert_eq!(
        prepared.link(0).map(|link| link.url.as_str()),
        Some("https://example.com/post")
    );
    let html = prepared.html().expect("legacy bodies render");
    assert!(!html.contains("blocked://"), "{html}");
}
