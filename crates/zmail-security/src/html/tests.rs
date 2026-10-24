use super::*;

fn sanitize(html: &str) -> SanitizedDocument {
    sanitize_html(html, &SanitizeOptions::default())
}

fn html_of(html: &str) -> String {
    sanitize(html).html.expect("the markup sanitises")
}

fn text_of(html: &str) -> String {
    sanitize(html).text
}

#[test]
fn keeps_table_layout_and_presentational_attributes() {
    let html = html_of(concat!(
        r##"<table width="600" align="center" bgcolor="#ffffff" border="0" cellpadding="8" cellspacing="0" style="border-collapse:collapse">"##,
        r#"<tr><td valign="top" colspan="2" rowspan="1" class="hero" nowrap>"#,
        r#"<font color="red" face="Arial" size="2"><b>Sale</b></font></td></tr></table>"#,
    ));
    assert_eq!(
        html,
        concat!(
            r##"<table width="600" align="center" bgcolor="#ffffff" border="0" cellpadding="8" cellspacing="0" style="border-collapse:collapse;">"##,
            r#"<tbody><tr><td valign="top" colspan="2" rowspan="1" class="hero" nowrap="">"#,
            r#"<font color="red" face="Arial" size="2"><b>Sale</b></font></td></tr></tbody></table>"#,
        )
    );
}

#[test]
fn drops_active_content_and_its_attributes() {
    let html = html_of(concat!(
        r#"<div onclick="alert(1)" onmouseover=alert(1) data-x="1">Hi"#,
        r#"<script>alert(1)</script><iframe src="https://evil.example/"></iframe>"#,
        r#"<object data="x.swf">obj</object><embed src="x.swf">"#,
        r#"<form action="https://evil.example/"><input name=q><button formaction=x>Go</button></form>"#,
        r#"<svg onload=alert(1)><image href="file:///etc/passwd"/></svg>"#,
        r#"<math><mtext>m</mtext></math><base href="https://evil.example/">"#,
        r#"<meta http-equiv="refresh" content="0;url=https://evil.example/">"#,
        r#"<link rel="stylesheet" href="https://evil.example/x.css">"#,
        r#"<a ping="https://evil.example/ping" target="_blank" href="https://ok.example/">ok</a>"#,
        r#"<!-- comment --></div>"#,
    ));
    assert_eq!(html, r#"<div>HiGo<a href="zmail-link:0">ok</a></div>"#);
}

#[test]
fn links_become_ids_and_keep_their_targets() {
    let document = sanitize(concat!(
        r#"<p><a href="http://plain.example/a">Plain</a> "#,
        r#"<a href="HTTPS://Secure.Example/b?x=1&amp;y=2">Secure</a> "#,
        r#"<a href="mailto:help@example.com">Mail</a> <a href="tel:+353 1 234">Call</a> "#,
        r##"<a href="javascript:alert(1)">Script</a> <a href="#top">Top</a> "##,
        r#"<a href="/relative">Relative</a> <a>Anchor</a></p>"#,
    ));
    let urls: Vec<&str> = document
        .links
        .iter()
        .map(|link| link.url.as_str())
        .collect();
    assert_eq!(
        urls,
        [
            "http://plain.example/a",
            "https://secure.example/b?x=1&y=2",
            "mailto:help@example.com",
            "tel:+353 1 234",
        ]
    );
    assert_eq!(document.links[0].scheme(), "http");
    let html = document.html.expect("sanitises");
    for id in 0..4 {
        assert!(
            html.contains(&format!("href=\"zmail-link:{id}\"")),
            "{html}"
        );
    }
    assert!(!html.contains("javascript"), "{html}");
    assert_eq!(
        document.text,
        "Plain Secure Mail Call Script Top Relative Anchor"
    );
    let texts: Vec<(&str, usize)> = document
        .text_links
        .iter()
        .map(|link| (&document.text[link.range.clone()], link.link))
        .collect();
    assert_eq!(
        texts,
        [("Plain", 0), ("Secure", 1), ("Mail", 2), ("Call", 3)]
    );
}

#[test]
fn remote_resources_are_blocked_and_listed() {
    let document = sanitize(concat!(
        r#"<table background="https://cdn.example.com/bg.png"><tr><td>"#,
        r#"<img src="https://cdn.example.com/a.png" alt="A">"#,
        r#"<img src="HTTP://track.example.net/p.gif?u=1" width="1" height="1">"#,
        r#"<img src="cid:logo@example.com"><img src="data:image/gif;base64,R0lGOD">"#,
        r#"<img src="data:image/svg+xml,<svg/>"><img src="file:///C:/x.png">"#,
        r#"<img srcset="https://cdn.example.com/2x.png 2x">"#,
        r#"</td></tr></table>"#,
    ));
    assert!(document.has_remote_content());
    let html = document.html.expect("sanitises");
    assert_eq!(
        html,
        concat!(
            r#"<table background="zmail-blocked:0"><tbody><tr><td>"#,
            r#"<img src="zmail-blocked:1" alt="A">"#,
            r#"<img src="zmail-blocked:2" width="1" height="1">"#,
            r#"<img src="cid:logo@example.com"><img src="data:image/gif;base64,R0lGOD">"#,
            r#"<img><img><img></td></tr></tbody></table>"#,
        )
    );
    let remote: Vec<(&str, &str, bool)> = document
        .remote
        .iter()
        .map(|resource| {
            (
                resource.url.as_str(),
                resource.origin.as_str(),
                resource.loads,
            )
        })
        .collect();
    assert_eq!(
        remote,
        [
            (
                "https://cdn.example.com/bg.png",
                "https://cdn.example.com",
                false
            ),
            (
                "https://cdn.example.com/a.png",
                "https://cdn.example.com",
                false
            ),
            (
                "http://track.example.net/p.gif?u=1",
                "http://track.example.net",
                false
            ),
        ]
    );
    assert_eq!(document.cid_references, ["logo@example.com"]);
}

#[test]
fn allowed_remote_content_keeps_public_https_only() {
    let document = sanitize_html(
        concat!(
            r#"<img src="https://cdn.example.com/a.png"><img src="http://cdn.example.com/b.png">"#,
            r#"<img src="https://192.168.1.10/c.png"><img src="https://intranet/d.png">"#,
            r#"<div style="background-image:url(https://cdn.example.com/e.png)">x</div>"#,
        ),
        &SanitizeOptions {
            allow_remote: true,
            ..SanitizeOptions::default()
        },
    );
    assert_eq!(
        document.html.as_deref(),
        Some(concat!(
            r#"<img src="https://cdn.example.com/a.png"><img src="zmail-blocked:1">"#,
            r#"<img src="zmail-blocked:2"><img src="zmail-blocked:3">"#,
            r#"<div style="background-image:url(&quot;https://cdn.example.com/e.png&quot;);">x</div>"#,
        ))
    );
}

#[test]
fn style_blocks_are_sanitised_and_kept_first() {
    let html = html_of(concat!(
        "<html><head><style>@import url(https://evil.example/x.css); ",
        ".btn { color: #fff; behavior: url(x.htc); background: url(https://t.example/b.png) }</style>",
        "<style>@media (max-width: 600px) { td { display: block } }</style></head>",
        "<body><p class=btn>Buy</p></body></html>",
    ));
    assert_eq!(
        html,
        concat!(
            "<style>.btn{color:#fff;background:url(\"zmail-blocked:0\");}\n",
            "@media (max-width: 600px){td{display:block;}}</style>",
            "<p class=\"btn\">Buy</p>",
        )
    );
}

#[test]
fn an_unclosed_style_block_does_not_swallow_the_next() {
    let html = html_of("<style>.a { color: red</style><style>.b { color: blue }</style>x");
    assert_eq!(html, "<style>.a{color:red;}\n.b{color:blue;}</style>x");
}

#[test]
fn the_body_tag_becomes_a_styled_wrapper() {
    let html = html_of(concat!(
        r##"<body bgcolor="#f4f4f4" text="#222" background="https://t.example/bg.png" "##,
        r#"style="margin:0; position:fixed" onload="alert(1)"><p>Hi</p></body>"#,
    ));
    assert_eq!(
        html,
        concat!(
            r#"<div style="background-color:#f4f4f4;color:#222;background-image:url(&quot;zmail-blocked:0&quot;);margin:0;">"#,
            "<p>Hi</p></div>",
        )
    );
    let html = html_of(r#"<body bgcolor="red;x:expression(1)"><p>Hi</p></body>"#);
    assert_eq!(html, "<p>Hi</p>");
}

#[test]
fn text_has_paragraphs_lines_and_cells() {
    assert_eq!(
        text_of(concat!(
            "<html><head><title>Newsletter</title></head><body>",
            "<h1>Welcome</h1><p>Your new <b>dash</b>board</p>",
            "<h2>Faster exports</h2>",
            "<table><tr><th>Plan</th><th>Price</th></tr>",
            "<tr><td>Starter</td><td>EUR&nbsp;9</td></tr>",
            "<tr><td>Team</td><td width=20>&nbsp;</td><td>EUR 29</td></tr></table>",
            "<ul><li>One</li><li>Two</li></ul><ol start=3><li>Three</li><li>Four</li></ol>",
            "<p>Line one<br>line two<br><br>New paragraph</p>",
            "</body></html>",
        )),
        concat!(
            "Welcome\n\nYour new dashboard\n\nFaster exports\n\n",
            "Plan  Price\nStarter  EUR 9\nTeam  EUR 29\n\n",
            "• One\n• Two\n\n3. Three\n4. Four\n\n",
            "Line one\nline two\n\nNew paragraph",
        )
    );
}

#[test]
fn text_reads_composed_mail_line_by_line() {
    // Gmail and Outlook compose a line per <div>, with <div><br></div>
    // for a blank line.
    assert_eq!(
        text_of(
            "<div dir=ltr><div>Hi Rob,</div><div><br></div><div>Thanks for the notes.</div>\
             <div>See you Tuesday.</div><div><br></div><div>Ann</div></div>"
        ),
        "Hi Rob,\n\nThanks for the notes.\nSee you Tuesday.\n\nAnn"
    );
}

#[test]
fn text_leaves_out_hidden_preheaders_and_spacers() {
    assert_eq!(
        text_of(concat!(
            r#"<div style="display: none !important; max-height:0">Preview text&zwnj;&nbsp;&zwnj;&nbsp;</div>"#,
            r#"<span style="mso-hide:all">Outlook only</span>"#,
            "<p>AT&T &amp; friends &#8212; it&#x2019;s &lt;fine&gt;&zwnj;!</p>",
            "<pre>  indented\n    code</pre>",
        )),
        "AT&T & friends \u{2014} it\u{2019}s <fine>!\n\n  indented\n    code"
    );
}

#[test]
fn link_text_never_spans_a_line_break() {
    let document = sanitize(r#"<a href="https://example.com/"><div>Read</div><div>more</div></a>"#);
    assert_eq!(document.text, "Read\nmore");
    let ranges: Vec<_> = document
        .text_links
        .iter()
        .map(|link| (link.range.clone(), link.link))
        .collect();
    assert_eq!(ranges, [(0..4, 0), (5..9, 0)]);
}

#[test]
fn stored_markup_reads_back_to_the_same_text_and_targets() {
    let source = concat!(
        "<p>Pay your <a href=\"http://evil.example/pay\">https://bank.example</a> bill.</p>",
        "<table><tr><td>A</td><td>B</td></tr></table>",
        "<img src=\"https://t.example/p.gif\">",
    );
    let stored = sanitize_html_for_storage(source);
    assert!(stored.has_remote_content);
    assert!(
        stored.html.contains("href=\"http://evil.example/pay\""),
        "{}",
        stored.html
    );
    assert!(stored.html.contains("zmail-blocked:0"), "{}", stored.html);
    assert_eq!(stored.text, "Pay your https://bank.example bill.\n\nA  B");
    let (text, links) = text_of_stored_html(&stored.html, false);
    assert_eq!(text, stored.text);
    assert_eq!(links.len(), 1);
    assert_eq!(&text[links[0].range.clone()], "https://bank.example");
    assert_eq!(links[0].target.url, "http://evil.example/pay");
}

#[test]
fn too_complex_mail_is_stored_as_its_text_with_links() {
    let source = format!(
        "{}<p>Deep <a href=\"mailto:a@example.com\">mail</a></p><p>Second</p>{}",
        "<div>".repeat(2_000),
        "</div>".repeat(2_000)
    );
    let document = sanitize(&source);
    assert_eq!(document.too_complex, Some(Complexity::TooDeep));
    assert_eq!(document.html, None);
    assert_eq!(document.text, "Deep mail\n\nSecond");
    let stored = sanitize_html_for_storage(&source);
    let (text, links) = text_of_stored_html(&stored.html, false);
    assert_eq!(text, "Deep mail\n\nSecond");
    assert_eq!(links[0].target.url, "mailto:a@example.com");
}

#[test]
fn legacy_bodies_read_with_their_blocked_links() {
    let (text, links) = text_of_stored_html(
        r#"<p>Pay your <a href="blocked://evil.example/pay">invoice</a> <img src="blocked://t.example/p.gif">"#,
        true,
    );
    assert_eq!(text, "Pay your invoice");
    assert_eq!(links[0].target.url, "https://evil.example/pay");
}

#[test]
fn caps_send_mail_to_the_text() {
    let large = format!("<p>{}</p>", "x".repeat(MAX_HTML_BYTES));
    let document = sanitize(&large);
    assert_eq!(document.too_complex, Some(Complexity::InputTooLarge));
    assert!(document.text.starts_with("xxx"));

    let many = "<span>a</span>".repeat(MAX_ELEMENTS + 1);
    assert_eq!(
        sanitize(&many).too_complex,
        Some(Complexity::TooManyElements)
    );

    let style = format!("<style>{}</style><p>Hi</p>", ".a{color:red}".repeat(30_000));
    let document = sanitize(&style);
    assert_eq!(document.too_complex, Some(Complexity::StyleTooLarge));
    assert_eq!(document.text, "Hi");

    // Just under the exact nesting cap still renders.
    let nested = format!(
        "{}x{}",
        "<div>".repeat(MAX_DEPTH),
        "</div>".repeat(MAX_DEPTH)
    );
    assert!(sanitize(&nested).html.is_some());
    let nested = format!(
        "{}x{}",
        "<div>".repeat(MAX_DEPTH + 1),
        "</div>".repeat(MAX_DEPTH + 1)
    );
    assert_eq!(sanitize(&nested).too_complex, Some(Complexity::TooDeep));
}

#[test]
fn sloppy_markup_is_not_mistaken_for_deep_nesting() {
    let sloppy = format!(
        "<table>{}</table>{}",
        "<tr><td>a<td>b".repeat(2_000),
        "<p>para".repeat(2_000)
    );
    let document = sanitize(&sloppy);
    assert_eq!(document.too_complex, None);
    assert!(document.html.is_some());
}
