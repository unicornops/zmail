//! The HTML sanitiser against hostile mail (ADR 0006, Phase 0 gate).
//!
//! Every fixture under `tests/fixtures/hostile`, generated pathological
//! markup and thousands of deterministic mutations of both go through
//! `sanitize_html`, and an independent checker re-reads each output with
//! html5ever's tokenizer: only allowlisted elements and attributes, no
//! event handlers, every `href` an opaque link id, every resource a
//! `cid:` part, an allowed `data:` image, a blocked placeholder or (when
//! remote content is allowed) a public `https:` address, and CSS without
//! imports, fonts, code, bindings, pinned positions or unpoliced `url()`.

use std::{cell::RefCell, fs, path::Path, time::Instant};

use html5ever::{
    tendril::StrTendril,
    tokenizer::{
        BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
        states::RawKind,
    },
};
use rand::{Rng, SeedableRng, rngs::StdRng, seq::SliceRandom};
use zmail_security::html::{
    MAX_DEPTH, MAX_ELEMENTS, MAX_HTML_BYTES, MAX_STYLE_BYTES, SanitizeOptions, SanitizedDocument,
    is_public_remote_url, sanitize_html, sanitize_html_for_storage, text_of_stored_html,
};

const ALLOWED_ELEMENTS: &[&str] = &[
    "a",
    "abbr",
    "address",
    "article",
    "aside",
    "b",
    "bdi",
    "bdo",
    "big",
    "blockquote",
    "br",
    "caption",
    "center",
    "cite",
    "code",
    "col",
    "colgroup",
    "dd",
    "del",
    "details",
    "dfn",
    "div",
    "dl",
    "dt",
    "em",
    "figcaption",
    "figure",
    "font",
    "footer",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "i",
    "img",
    "ins",
    "kbd",
    "li",
    "main",
    "mark",
    "nav",
    "ol",
    "p",
    "pre",
    "q",
    "rp",
    "rt",
    "ruby",
    "s",
    "samp",
    "section",
    "small",
    "span",
    "strike",
    "strong",
    "style",
    "sub",
    "summary",
    "sup",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "time",
    "tr",
    "tt",
    "u",
    "ul",
    "var",
    "wbr",
];

const ALLOWED_ATTRIBUTES: &[&str] = &[
    "abbr",
    "align",
    "alt",
    "axis",
    "background",
    "bgcolor",
    "border",
    "cellpadding",
    "cellspacing",
    "class",
    "clear",
    "color",
    "colspan",
    "datetime",
    "dir",
    "face",
    "frame",
    "headers",
    "height",
    "href",
    "hspace",
    "id",
    "lang",
    "noshade",
    "nowrap",
    "open",
    "reversed",
    "role",
    "rowspan",
    "rules",
    "scope",
    "size",
    "span",
    "src",
    "start",
    "style",
    "summary",
    "title",
    "type",
    "valign",
    "value",
    "vspace",
    "width",
];

/// Fragments the mutation loop splices in: the pieces attacks are made of.
const DANGEROUS_PIECES: &[&str] = &[
    "<script>alert(1)</script>",
    "<img src=x onerror=alert(1)>",
    "<svg onload=alert(1)>",
    "<iframe src=https://evil.example/>",
    "<a href=\"javascript:alert(1)\">",
    "<a href='https://ok.example/'>",
    "<style>",
    "</style>",
    "<style>.x{background:url(https://evil.example/s.png)}</style>",
    "<div style=\"background:url(https://evil.example/d.png)\">",
    "<div style=\"position:fixed\">",
    "<table background=\"https://evil.example/t.png\">",
    "<img src=\"cid:part@x\">",
    "<img src=\"https://evil.example/p.gif\">",
    "<img src=\"file:///etc/passwd\">",
    "<img src=\"\\\\evil\\share\\x.png\">",
    "<base href=https://evil.example/>",
    "<meta http-equiv=refresh content=0;url=https://evil.example/>",
    "<math><mtext><table><mglyph><style>",
    "<noscript>",
    "</noscript>",
    "<textarea>",
    "<template>",
    "<!--",
    "-->",
    "<![CDATA[",
    "]]>",
    "\"",
    "'",
    "=",
    "<",
    ">",
    "/",
    "\\",
    "&#x3c;",
    "&lt;",
    "&#0;",
    "\u{0}",
    "\u{feff}",
    "url(",
    "expression(",
    "@import ",
    "\\75 rl(",
    "/**/",
    "</p>",
    "<p>",
    "<td>",
    "</table>",
    "<body bgcolor=red onload=alert(1)>",
];

#[derive(Default)]
struct Checker {
    errors: Vec<String>,
    raw_text: Option<String>,
    style_count: usize,
    started: bool,
}

struct CheckerSink {
    checker: RefCell<Checker>,
    allow_remote: bool,
}

impl TokenSink for CheckerSink {
    type Handle = ();

    fn process_token(&self, token: Token, _line: u64) -> TokenSinkResult<()> {
        let mut checker = self.checker.borrow_mut();
        let first = !checker.started;
        checker.started = true;
        match token {
            Token::TagToken(tag) if tag.kind == TagKind::StartTag => {
                let name: &str = &tag.name;
                if !ALLOWED_ELEMENTS.contains(&name) {
                    checker.errors.push(format!("element <{name}>"));
                }
                if name == "style" {
                    checker.style_count += 1;
                    if !first || checker.style_count > 1 {
                        checker.errors.push("a <style> not first".to_owned());
                    }
                    if !tag.attrs.is_empty() {
                        checker.errors.push("attributes on <style>".to_owned());
                    }
                    checker.raw_text = Some("style".to_owned());
                    return TokenSinkResult::RawData(RawKind::Rawtext);
                }
                for attribute in &tag.attrs {
                    let attribute_name: &str = &attribute.name.local;
                    let value: &str = &attribute.value;
                    if attribute.name.prefix.is_some()
                        || (!ALLOWED_ATTRIBUTES.contains(&attribute_name)
                            && !attribute_name.starts_with("aria-"))
                    {
                        checker
                            .errors
                            .push(format!("attribute {attribute_name} on <{name}>"));
                    }
                    if let Some(error) =
                        check_attribute(name, attribute_name, value, self.allow_remote)
                    {
                        checker.errors.push(error);
                    }
                }
            }
            Token::TagToken(tag) => {
                if checker.raw_text.as_deref() == Some(&*tag.name) {
                    checker.raw_text = None;
                }
            }
            Token::CharacterTokens(text) if checker.raw_text.is_some() => {
                if let Some(error) = check_css(&text, self.allow_remote) {
                    checker.errors.push(format!("stylesheet: {error}"));
                }
            }
            Token::CommentToken(_) => checker.errors.push("a comment".to_owned()),
            Token::DoctypeToken(_) => checker.errors.push("a doctype".to_owned()),
            Token::NullCharacterToken => checker.errors.push("a NUL".to_owned()),
            _ => {}
        }
        TokenSinkResult::Continue
    }
}

fn check_attribute(element: &str, name: &str, value: &str, allow_remote: bool) -> Option<String> {
    let bad = match name {
        "href" => {
            element != "a"
                || !value
                    .strip_prefix("zmail-link:")
                    .is_some_and(|id| !id.is_empty() && id.chars().all(|ch| ch.is_ascii_digit()))
        }
        "src" => element != "img" || !is_policed_resource(value, true, allow_remote),
        "background" => !is_policed_resource(value, false, allow_remote),
        "style" => return check_css(value, allow_remote).map(|error| format!("style: {error}")),
        // Other values are text (alt, title, colours, sizes): the
        // attribute allowlist keeps out every attribute that loads.
        _ => false,
    };
    bad.then(|| format!("{name}={value:?} on <{element}>"))
}

fn is_policed_resource(value: &str, image: bool, allow_remote: bool) -> bool {
    if let Some(content_id) = value.strip_prefix("cid:") {
        return !content_id.is_empty()
            && content_id
                .chars()
                .all(|ch| ch.is_ascii_graphic() && !matches!(ch, '"' | '\'' | '\\' | '<' | '>'));
    }
    if let Some(index) = value.strip_prefix("zmail-blocked:") {
        return !index.is_empty() && index.chars().all(|ch| ch.is_ascii_digit());
    }
    if image {
        let lower = value.to_ascii_lowercase();
        if ["png", "gif", "jpeg", "webp"].iter().any(|kind| {
            lower.starts_with(&format!("data:image/{kind};"))
                || lower.starts_with(&format!("data:image/{kind},"))
        }) {
            return true;
        }
    }
    allow_remote && is_public_remote_url(value)
}

/// What no sanitised CSS may contain, read token by token: rule-level
/// at-rules other than `@media` and `@supports`, functions that run code
/// or fetch without `url()`, `url()` without a policed address,
/// `behavior`/`binding` properties, pinned positions and any `<`.
fn check_css(css: &str, allow_remote: bool) -> Option<String> {
    if css.contains('<') {
        return Some(format!("a '<' in {css:?}"));
    }
    let lower = css.to_ascii_lowercase();
    // Strings are text (font names, `content`); url() reads the ones that
    // are addresses from the original.
    let code = without_strings(&lower);
    // An escaped character (`\:`, `\ `) is part of the name around it.
    let code = blank_escapes(&code);
    // Non-ASCII characters are name characters in CSS.
    let is_name =
        |ch: char| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '\\') || !ch.is_ascii();
    let name_before = |end: usize| {
        let start = code[..end]
            .char_indices()
            .rev()
            .take_while(|&(_, ch)| is_name(ch))
            .last()
            .map_or(end, |(index, _)| index);
        (&code[start..end], code[..start].chars().next_back())
    };

    for (at, _) in code.match_indices('(') {
        let (name, before) = name_before(at);
        if name.is_empty() || matches!(before, Some('#' | '@')) {
            continue;
        }
        if [
            "expression",
            "image-set",
            "cross-fade",
            "element",
            "image",
            "paint",
            "attr",
        ]
        .iter()
        .any(|denied| name == *denied || name.ends_with(&format!("-{denied}")))
        {
            return Some(format!("{name}() in {css:?}"));
        }
        if name == "url" || name == "src" {
            let Some(quoted) = css[at + 1..].strip_prefix('"') else {
                return Some(format!("an unquoted {name}( in {css:?}"));
            };
            let Some(end) = quoted.find('"') else {
                return Some(format!("an unterminated {name}( in {css:?}"));
            };
            if !is_policed_resource(&quoted[..end], false, allow_remote) {
                return Some(format!("{name}({:?}) in {css:?}", &quoted[..end]));
            }
        }
    }

    for (at, _) in code.match_indices('@') {
        let rest = &code[at + 1..];
        let name: String = rest.chars().take_while(|&ch| is_name(ch)).collect();
        let rule_level = code[..at]
            .trim_end()
            .chars()
            .next_back()
            .is_none_or(|ch| matches!(ch, '{' | '}'));
        if rule_level && name != "media" && name != "supports" {
            return Some(format!("@{name} in {css:?}"));
        }
    }

    for declaration in code.split(['{', '}', ';']) {
        let Some((property, value)) = declaration.split_once(':') else {
            continue;
        };
        let property = property.trim();
        if property.contains("behavior") || property.contains("binding") {
            return Some(format!("a {property} declaration in {css:?}"));
        }
        if property == "position" && (value.contains("fixed") || value.contains("sticky")) {
            return Some(format!("position:{value} in {css:?}"));
        }
    }
    None
}

/// CSS with each escape (`\` and the character after it) turned into
/// name characters, byte for byte.
fn blank_escapes(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut chars = css.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            out.push('_');
            if let Some(escaped) = chars.next() {
                out.extend(std::iter::repeat_n('_', escaped.len_utf8()));
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// CSS with the contents of its quoted strings blanked, byte for byte.
fn without_strings(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut quote = None;
    let mut escaped = false;
    for ch in css.chars() {
        match quote {
            Some(open) if escaped || (ch != open && ch != '\\') => {
                escaped = false;
                out.extend(std::iter::repeat_n(' ', ch.len_utf8()));
            }
            Some(_) if ch == '\\' => {
                escaped = true;
                out.push(' ');
            }
            Some(_) => {
                quote = None;
                out.push(ch);
            }
            None => {
                if ch == '"' || ch == '\'' {
                    quote = Some(ch);
                }
                out.push(ch);
            }
        }
    }
    out
}

/// Every problem the checker finds in sanitised HTML.
fn check_html(html: &str, allow_remote: bool) -> Vec<String> {
    let sink = CheckerSink {
        checker: RefCell::new(Checker::default()),
        allow_remote,
    };
    let queue = BufferQueue::default();
    queue.push_back(StrTendril::from_slice(html));
    let tokenizer = Tokenizer::new(sink, TokenizerOpts::default());
    let _ = tokenizer.feed(&queue);
    tokenizer.end();
    tokenizer.sink.checker.into_inner().errors
}

/// Sanitises `input` both ways and checks the results; returns the
/// blocked-content document.
fn assert_safe(label: &str, input: &str) -> SanitizedDocument {
    let mut blocked = None;
    for allow_remote in [false, true] {
        let document = sanitize_html(
            input,
            &SanitizeOptions {
                allow_remote,
                legacy_blocked_links: false,
            },
        );
        if let Some(html) = &document.html {
            let errors = check_html(html, allow_remote);
            assert!(
                errors.is_empty(),
                "{label} (remote allowed: {allow_remote}): {errors:#?}\n--- output ---\n{html}"
            );
            for resource in &document.remote {
                if resource.loads {
                    assert!(allow_remote && is_public_remote_url(&resource.url));
                }
            }
        }
        for link in &document.text_links {
            assert!(link.range.end <= document.text.len(), "{label}");
            assert!(document.text.is_char_boundary(link.range.start), "{label}");
            assert!(document.text.is_char_boundary(link.range.end), "{label}");
            assert!(link.link < document.links.len(), "{label}");
            assert!(!document.text[link.range.clone()].contains('\n'), "{label}");
        }
        for link in &document.links {
            assert!(
                ["http", "https", "mailto", "tel", "sms"].contains(&link.scheme()),
                "{label}: {link:?}"
            );
        }
        if !allow_remote {
            blocked = Some(document);
        }
    }

    // The stored form re-reads to the same text and is itself safe.
    let stored = sanitize_html_for_storage(input);
    let (text, _) = text_of_stored_html(&stored.html, false);
    let stored_text = stored.text.clone();
    assert_eq!(text, stored_text, "{label}: stored text");
    blocked.expect("the blocked pass ran")
}

fn fixtures() -> Vec<(String, String)> {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hostile");
    let mut fixtures: Vec<(String, String)> = fs::read_dir(&directory)
        .expect("the hostile fixtures exist")
        .map(|entry| {
            let path = entry.expect("a fixture").path();
            (
                path.file_name()
                    .expect("a file name")
                    .to_string_lossy()
                    .into_owned(),
                fs::read_to_string(&path).expect("a UTF-8 fixture"),
            )
        })
        .collect();
    fixtures.sort();
    assert!(fixtures.len() >= 6, "the corpus went missing");
    fixtures
}

#[test]
fn hostile_fixtures_come_out_safe() {
    for (name, html) in fixtures() {
        let document = assert_safe(&name, &html);
        let html_out = document.html.as_deref().unwrap_or_default();
        // Attack text may survive as escaped text; addresses never do.
        for leak in ["evil.example", "track.example.net", "attacker"] {
            assert!(
                !html_out.contains(leak),
                "{name}: {leak} survived\n{html_out}"
            );
        }
    }
}

#[test]
fn the_mxss_fixture_leaves_no_executable_markup() {
    let (_, html) = fixtures()
        .into_iter()
        .find(|(name, _)| name == "mxss.html")
        .expect("the mXSS fixture");
    let document = assert_safe("mxss.html", &html);
    let output = document.html.expect("sanitises");
    // Attack text may survive as escaped text, never as markup.
    assert!(!output.contains("<img src=x"), "{output}");
    assert!(!output.contains("onerror="), "{output}");
}

#[test]
fn hostile_fixtures_text_is_readable() {
    let fixtures = fixtures();
    let xss = &fixtures
        .iter()
        .find(|(name, _)| name == "xss-vectors.html")
        .expect("the XSS fixture")
        .1;
    let document = sanitize_html(xss, &SanitizeOptions::default());
    assert!(document.text.starts_with("Before"), "{}", document.text);
    assert!(document.text.ends_with("After"), "{}", document.text);
    // Only the bogus-comment CDATA line leaves script text behind, as
    // text.
    assert_eq!(
        document.text.matches("alert").count(),
        1,
        "{}",
        document.text
    );
    assert!(document.text.contains("alert(28)]]>"), "{}", document.text);
    assert_eq!(
        document
            .links
            .iter()
            .map(|link| link.url.as_str())
            .collect::<Vec<_>>(),
        ["https://ok.example/", "https://ok.example/"]
    );
}

#[test]
fn deeply_nested_tables_cost_nothing_and_fall_back_to_text() {
    // The Blitz spike's 1,000-deep table mail aborted the process.
    let nested = format!(
        "{}<a href=\"https://example.com/\">bottom</a>{}",
        "<table><tr><td>".repeat(1_000),
        "</td></tr></table>".repeat(1_000)
    );
    let started = Instant::now();
    let document = assert_safe("1,000 nested tables", &nested);
    assert!(document.html.is_none());
    assert_eq!(document.text, "bottom");
    assert_eq!(document.links[0].url, "https://example.com/");

    for (label, markup) in [
        ("100,000 nested divs", "<div>".repeat(100_000)),
        ("100,000 unclosed bold", "<b>".repeat(100_000)),
        ("100,000 nested spans closed", {
            let mut markup = "<span>".repeat(100_000);
            markup.push_str(&"</span>".repeat(100_000));
            markup
        }),
        (
            "60,000 misnested formatting elements",
            "<a><b><i><u><s>x</a>".repeat(12_000),
        ),
    ] {
        assert_safe(label, &markup);
    }
    let depth = MAX_DEPTH / 4;
    let just_under = format!(
        "{}x{}",
        "<table><tr><td>".repeat(depth),
        "</td></tr></table>".repeat(depth)
    );
    assert!(
        sanitize_html(&just_under, &SanitizeOptions::default())
            .html
            .is_some()
    );
    assert!(
        started.elapsed().as_secs() < 60,
        "pathological nesting took {:?}",
        started.elapsed()
    );
}

#[test]
fn oversized_mail_falls_back_to_text() {
    let huge_style = format!(
        "<style>{}</style><p>Hello</p>",
        ".a{color:red}".repeat(MAX_STYLE_BYTES / 10)
    );
    let document = assert_safe("huge style", &huge_style);
    assert!(document.html.is_none());
    assert_eq!(document.text, "Hello");

    let many = format!("{}<p>end</p>", "<i>a</i>".repeat(MAX_ELEMENTS));
    assert!(assert_safe("too many elements", &many).html.is_none());

    let large = format!(
        "<p>{}</p><img src=\"https://t.example/x.png\">",
        "word ".repeat(MAX_HTML_BYTES / 5)
    );
    let document = assert_safe("too large", &large);
    assert!(document.html.is_none());
    assert!(document.remote.is_empty());
}

#[test]
fn mutated_hostile_mail_comes_out_safe() {
    let mut corpus: Vec<String> = fixtures().into_iter().map(|(_, html)| html).collect();
    corpus.extend(DANGEROUS_PIECES.iter().map(|piece| (*piece).to_owned()));
    corpus.push(
        "<table><tr><td style=\"background:url(cid:x)\"><a href=\"https://a.example/\">a</a></td></tr></table>"
            .to_owned(),
    );
    // CI runs a fixed seed; a longer local run can pick its own:
    // ZMAIL_FUZZ_SEED=7 ZMAIL_FUZZ_ITERATIONS=1000000 cargo test --release …
    let seed = std::env::var("ZMAIL_FUZZ_SEED")
        .ok()
        .and_then(|seed| seed.parse().ok())
        .unwrap_or(0x0006_0000_7a6d_6169);
    let iterations = std::env::var("ZMAIL_FUZZ_ITERATIONS")
        .ok()
        .and_then(|iterations| iterations.parse().ok())
        .unwrap_or(if cfg!(debug_assertions) {
            1_000
        } else {
            10_000
        });
    let mut rng = StdRng::seed_from_u64(seed);
    for iteration in 0..iterations {
        let base = corpus.choose(&mut rng).expect("a corpus entry");
        let mut mutated = base.clone();
        for _ in 0..rng.gen_range(1..=6) {
            mutate(&mut mutated, &corpus, &mut rng);
        }
        // Keep inputs small enough that the loop stays fast.
        if mutated.len() > 64 * 1024 {
            let cut = mutated.floor_char_boundary(64 * 1024);
            mutated.truncate(cut);
        }
        assert_safe(&format!("mutation {iteration} of seed {seed}"), &mutated);
    }
}

fn mutate(input: &mut String, corpus: &[String], rng: &mut StdRng) {
    let boundary = |input: &str, rng: &mut StdRng| {
        let at = rng.gen_range(0..=input.len());
        input.floor_char_boundary(at)
    };
    match rng.gen_range(0..6) {
        0 | 1 => {
            let at = boundary(input, rng);
            let piece = DANGEROUS_PIECES.choose(rng).expect("a piece");
            input.insert_str(at, piece);
        }
        2 => {
            let start = boundary(input, rng);
            let end = start + input[start..].len().min(rng.gen_range(0..64));
            let end = input.floor_char_boundary(end);
            input.replace_range(start..end, "");
        }
        3 => {
            let start = boundary(input, rng);
            let end =
                input.floor_char_boundary(start + input[start..].len().min(rng.gen_range(0..128)));
            let copy = input[start..end].to_owned();
            let at = boundary(input, rng);
            input.insert_str(at, &copy);
        }
        4 => {
            let other = corpus.choose(rng).expect("a corpus entry");
            let from = boundary(other, rng);
            let to =
                other.floor_char_boundary(from + other[from..].len().min(rng.gen_range(0..256)));
            let at = boundary(input, rng);
            input.insert_str(at, &other[from..to]);
        }
        _ => {
            let start = boundary(input, rng);
            let end =
                input.floor_char_boundary(start + input[start..].len().min(rng.gen_range(0..32)));
            let flipped: String = input[start..end]
                .chars()
                .map(|ch| {
                    if ch.is_ascii_lowercase() {
                        ch.to_ascii_uppercase()
                    } else {
                        ch.to_ascii_lowercase()
                    }
                })
                .collect();
            input.replace_range(start..end, &flipped);
        }
    }
}
