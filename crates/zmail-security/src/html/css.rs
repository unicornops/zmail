//! The sanitiser's CSS pass, over `style` attributes and `<style>` blocks.
//!
//! CSS is tokenised with cssparser and written back token by token, so
//! escapes (`\75 rl(`) and comments (`u/**/rl(`) cannot smuggle anything
//! past the checks, which all run on decoded tokens:
//!
//! - only style rules, `@media` and `@supports` survive; every other
//!   at-rule (`@import`, `@font-face`, `@charset`, `@namespace`,
//!   `@keyframes`…) is dropped with its block;
//! - a declaration is dropped for `behavior`/`-moz-binding`, for `position:
//!   fixed` or `sticky` (which could pin mail over the reader), for a bad
//!   string or URL token, and for functions that run code or fetch without
//!   `url()` (`expression()`, `image-set()`, `element()`…);
//! - every `url()` goes through the remote-content policy: blocked remote
//!   addresses become `url("zmail-blocked:<n>")`, `cid:` parts stay, and
//!   anything else drops the declaration.
//!
//! The output never contains `<`, so a stylesheet cannot close its
//! `<style>` element.

use std::fmt::Write as _;

use cssparser::{Delimiter, ParseError, Parser, ToCss, Token, serialize_identifier};

use super::policy::{ResourceUse, UrlPolicy};

/// How deep functions and blocks may nest inside one value or selector.
const MAX_VALUE_NESTING: usize = 16;

/// How deep `@media` and `@supports` may nest.
const MAX_RULE_NESTING: usize = 4;

/// Functions that run code, reach outside the message or fetch an address
/// without `url()`.
const DENIED_FUNCTIONS: &[&str] = &[
    "expression",
    "image-set",
    "image",
    "element",
    "cross-fade",
    "paint",
    "canvas",
    "attr",
];

/// A denied function, or a prefixed form of one (`-webkit-image-set`).
fn is_denied_function(name: &str) -> bool {
    DENIED_FUNCTIONS.iter().any(|denied| {
        name.strip_suffix(denied)
            .is_some_and(|prefix| prefix.is_empty() || prefix.ends_with('-'))
    })
}

/// A declaration or rule the pass leaves out.
struct Rejected;

type Nested = Result<(), ParseError<()>>;

/// The parse error that makes cssparser skip the rest of a declaration
/// or rule.
fn rejected() -> ParseError<()> {
    ParseError::custom(())
}

/// Sanitises a stylesheet (the contents of `<style>` blocks).
pub(crate) fn sanitize_stylesheet(css: &str, policy: &mut UrlPolicy) -> String {
    let mut parser = Parser::new(css);
    let mut out = String::new();
    rule_list(&mut parser, policy, &mut out, 0);
    finish(out)
}

/// Sanitises a declaration list (a `style` attribute).
pub(crate) fn sanitize_declarations(css: &str, policy: &mut UrlPolicy) -> String {
    let mut parser = Parser::new(css);
    let mut out = String::new();
    declaration_list(&mut parser, policy, &mut out);
    finish(out)
}

/// The last line of defence for `<style>`: no `<`, so no `</style>`. A
/// `<` can only come from a string or an escaped identifier, where the
/// CSS escape `\3c ` means the same.
fn finish(out: String) -> String {
    let out = out.trim();
    if out.contains('<') {
        out.replace('<', "\\3c ")
    } else {
        out.to_owned()
    }
}

fn rule_list(parser: &mut Parser<'_>, policy: &mut UrlPolicy, out: &mut String, depth: usize) {
    loop {
        let state = parser.state();
        let token = match parser.next() {
            Ok(token) => token.clone(),
            Err(_) => return,
        };
        match token {
            Token::CDO | Token::CDC | Token::Semicolon => {}
            Token::AtKeyword(name) => at_rule(parser, &name, policy, out, depth),
            _ => {
                parser.reset(&state);
                qualified_rule(parser, policy, out);
            }
        }
    }
}

/// An at-rule whose `@name` was just read: `@media` and `@supports` keep
/// their (sanitised) rules; everything else is skipped, block and all.
fn at_rule(
    parser: &mut Parser<'_>,
    name: &str,
    policy: &mut UrlPolicy,
    out: &mut String,
    depth: usize,
) {
    let name = name.to_ascii_lowercase();
    if !matches!(name.as_str(), "media" | "supports") || depth >= MAX_RULE_NESTING {
        skip_statement(parser);
        return;
    }
    let mut prelude = String::new();
    let prelude_ok = parser
        .parse_until_before(Delimiter::CurlyBracketBlock | Delimiter::Semicolon, |p| {
            component_values(p, policy, &mut prelude, 0, false).map_err(|_| rejected())
        })
        .is_ok();
    if !matches!(parser.next(), Ok(Token::CurlyBracketBlock)) || !prelude_ok {
        // A statement (`@media x;`) or a broken prelude: a following block
        // is skipped by the next read.
        return;
    }
    let mut rules = String::new();
    let _ = parser.parse_nested_block(|p| -> Nested {
        rule_list(p, policy, &mut rules, depth + 1);
        Ok(())
    });
    if !rules.is_empty() {
        let _ = write!(out, "@{name} {}{{{rules}}}", prelude.trim());
    }
}

/// Skips the rest of an at-rule: up to its `;`, or past its block.
fn skip_statement(parser: &mut Parser<'_>) {
    while let Ok(token) = parser.next() {
        if matches!(token, Token::Semicolon | Token::CurlyBracketBlock) {
            return;
        }
    }
}

/// A style rule: its selector, then its declarations.
fn qualified_rule(parser: &mut Parser<'_>, policy: &mut UrlPolicy, out: &mut String) {
    let mut selector = String::new();
    let selector_ok = parser
        .parse_until_before(Delimiter::CurlyBracketBlock, |p| {
            component_values(p, policy, &mut selector, 0, false).map_err(|_| rejected())
        })
        .is_ok();
    if !matches!(parser.next(), Ok(Token::CurlyBracketBlock)) {
        return;
    }
    let mut declarations = String::new();
    let _ = parser.parse_nested_block(|p| -> Nested {
        declaration_list(p, policy, &mut declarations);
        Ok(())
    });
    let selector = selector.trim();
    if selector_ok && !selector.is_empty() && !declarations.is_empty() {
        let _ = write!(out, "{selector}{{{declarations}}}");
    }
}

fn declaration_list(parser: &mut Parser<'_>, policy: &mut UrlPolicy, out: &mut String) {
    loop {
        let token = match parser.next() {
            Ok(token) => token.clone(),
            Err(_) => return,
        };
        let Token::Ident(name) = token else {
            if !matches!(token, Token::Semicolon) {
                skip_declaration(parser);
            }
            continue;
        };
        let lower = name.to_ascii_lowercase();
        let checkpoint = policy.checkpoint();
        let mut value = String::new();
        let parsed = parser.parse_until_after(Delimiter::Semicolon, |p| {
            p.expect_colon()?;
            if is_denied_property(&lower) {
                return Err(rejected());
            }
            component_values(p, policy, &mut value, 0, true).map_err(|_| rejected())
        });
        let value = value.trim();
        if parsed.is_err() || value.is_empty() || is_denied_value(&lower, value) {
            policy.rollback(checkpoint);
            continue;
        }
        if lower.starts_with("--") {
            // Custom properties are case-sensitive.
            let _ = serialize_identifier(&name, out);
        } else {
            out.push_str(&lower);
        }
        out.push(':');
        out.push_str(value);
        out.push(';');
    }
}

fn skip_declaration(parser: &mut Parser<'_>) {
    let _ = parser.parse_until_after(Delimiter::Semicolon, |p| -> Nested {
        while p.next().is_ok() {}
        Ok(())
    });
}

fn is_denied_property(name: &str) -> bool {
    name.contains("behavior") || name.contains("binding")
}

/// `position: fixed` or `sticky` would pin the mail over the reader; a
/// `var()` could hide either.
fn is_denied_value(name: &str, value: &str) -> bool {
    if name != "position" {
        return false;
    }
    let value = value.to_ascii_lowercase();
    value.contains("fixed") || value.contains("sticky") || value.contains("var(")
}

/// Copies component values until `parser` is exhausted, policing every
/// URL (`urls` is false where a URL can only be an attack, such as a
/// selector or a media query).
fn component_values(
    parser: &mut Parser<'_>,
    policy: &mut UrlPolicy,
    out: &mut String,
    depth: usize,
    urls: bool,
) -> Result<(), Rejected> {
    loop {
        let token = match parser.next_including_whitespace_and_comments() {
            Ok(token) => token.clone(),
            Err(_) => return Ok(()),
        };
        match token {
            // A comment between two tokens keeps them apart.
            Token::WhiteSpace(_) | Token::Comment(_) => out.push(' '),
            Token::UnquotedUrl(url) if urls => push_url(out, policy, &url)?,
            Token::Function(ref name) => {
                let lower = name.to_ascii_lowercase();
                // Function names are ASCII: `\u{feff}url(` is a different
                // function from `url(`, and no reader should have to tell.
                if depth >= MAX_VALUE_NESTING || is_denied_function(&lower) || !name.is_ascii() {
                    return Err(Rejected);
                }
                if lower == "url" || lower == "src" {
                    if !urls {
                        return Err(Rejected);
                    }
                    let url = parser
                        .parse_nested_block(|p| {
                            p.expect_string_cloned()
                                .map(|url| url.to_string())
                                .map_err(Into::into)
                        })
                        .map_err(|_: ParseError<()>| Rejected)?;
                    push_url(out, policy, &url)?;
                } else {
                    let _ = token.to_css(out);
                    nested(parser, policy, out, depth, urls)?;
                    out.push(')');
                }
            }
            Token::ParenthesisBlock | Token::SquareBracketBlock => {
                if depth >= MAX_VALUE_NESTING {
                    return Err(Rejected);
                }
                let close = if token == Token::ParenthesisBlock {
                    ')'
                } else {
                    ']'
                };
                let _ = token.to_css(out);
                nested(parser, policy, out, depth, urls)?;
                out.push(close);
            }
            Token::QuotedString(value) => {
                let mut quoted = String::new();
                let _ = cssparser::serialize_string(&value, &mut quoted);
                out.push_str(&quoted.replace('<', "\\3c "));
            }
            // No identifier in real CSS contains `<`, and a `<` delimiter
            // only starts `<!--`; refusing them keeps `</style` out.
            Token::Ident(ref name)
            | Token::AtKeyword(ref name)
            | Token::Hash(ref name)
            | Token::IDHash(ref name)
                if name.contains('<') =>
            {
                return Err(Rejected);
            }
            Token::Dimension { ref unit, .. } if unit.contains('<') => return Err(Rejected),
            // A block inside a value could be read as a nested rule, out
            // of reach of the declaration checks; `@` and `<` never
            // belong in a value or selector.
            Token::CurlyBracketBlock | Token::Delim('<' | '@') => return Err(Rejected),
            Token::UnquotedUrl(_)
            | Token::BadUrl(_)
            | Token::BadString(_)
            | Token::CDO
            | Token::CDC
            | Token::CloseParenthesis
            | Token::CloseSquareBracket
            | Token::CloseCurlyBracket => return Err(Rejected),
            other => {
                let _ = other.to_css(out);
            }
        }
    }
}

fn nested(
    parser: &mut Parser<'_>,
    policy: &mut UrlPolicy,
    out: &mut String,
    depth: usize,
    urls: bool,
) -> Result<(), Rejected> {
    parser
        .parse_nested_block(|p| {
            component_values(p, policy, out, depth + 1, urls).map_err(|_| rejected())
        })
        .map_err(|_: ParseError<()>| Rejected)
}

fn push_url(out: &mut String, policy: &mut UrlPolicy, url: &str) -> Result<(), Rejected> {
    let url = policy
        .resource(url, ResourceUse::Background)
        .ok_or(Rejected)?;
    out.push_str("url(");
    let _ = cssparser::serialize_string(&url, out);
    out.push(')');
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html::SanitizeOptions;

    fn declarations(css: &str) -> String {
        sanitize_declarations(css, &mut UrlPolicy::new(&SanitizeOptions::default()))
    }

    fn stylesheet(css: &str) -> String {
        sanitize_stylesheet(css, &mut UrlPolicy::new(&SanitizeOptions::default()))
    }

    #[test]
    fn keeps_ordinary_declarations() {
        assert_eq!(
            declarations(
                "COLOR: #333; font: 14px/1.4 'Helvetica Neue', Arial !important;padding:0 8px"
            ),
            "color:#333;font:14px/1.4 \"Helvetica Neue\", Arial !important;padding:0 8px;"
        );
        assert_eq!(
            declarations("--Brand: rgb(0 0 0 / 50%); color: var(--Brand)"),
            "--Brand:rgb(0 0 0 / 50%);color:var(--Brand);"
        );
    }

    #[test]
    fn drops_code_bindings_and_pinned_positions() {
        for css in [
            "width: expression(alert(1))",
            "width: \\65 xpression(alert(1))",
            "behavior: url(x.htc)",
            "-moz-binding: url(x.xml#x)",
            "position: fixed",
            "POSITION: Sticky",
            "position: -webkit-sticky",
            "position: var(--p)",
            "background: image-set('https://t.example/a.png' 1x)",
            "background: -webkit-image-set('https://t.example/a.png' 1x)",
            "background: url(javascript:alert(1))",
            "background: url('data:image/png;base64,AAAA')",
            "background: url(file:///etc/passwd)",
            "background: url(\\\\server\\share\\x.png)",
            "background: url(\"x\" crossorigin)",
            "background: src(var(--u))",
            "content: attr(title)",
        ] {
            assert_eq!(declarations(css), "", "{css}");
        }
        assert_eq!(
            declarations("position: absolute; top: 0; position: fixed"),
            "position:absolute;top:0;"
        );
    }

    #[test]
    fn blocks_inside_values_cannot_smuggle_rules() {
        assert_eq!(
            stylesheet(
                ".x { a:hover { position: fixed } color: red; } @{color:red} .y{color:blue}"
            ),
            ".y{color:blue;}"
        );
        assert_eq!(declarations("--x: { a: b }; color: red"), "color:red;");
        assert_eq!(
            declarations("background: \u{feff}url(https://t.example/a.png); color: red"),
            "color:red;"
        );
        assert_eq!(
            declarations("font-family: 微软雅黑, sans-serif"),
            "font-family:微软雅黑, sans-serif;"
        );
        assert_eq!(
            declarations("background: webkit-image-set(url(https://t.example/a.png) 1x)"),
            ""
        );
    }

    #[test]
    fn rewrites_urls_through_the_policy() {
        let mut policy = UrlPolicy::new(&SanitizeOptions::default());
        assert_eq!(
            sanitize_declarations(
                "background: #fff url( 'https://t.example/bg.png' ) no-repeat; \
                 list-style-image: u\\72l(https://t.example/b.png); \
                 background-image: URL(cid:hero@x)",
                &mut policy
            ),
            "background:#fff url(\"zmail-blocked:0\") no-repeat;\
             list-style-image:url(\"zmail-blocked:1\");\
             background-image:url(\"cid:hero@x\");"
        );
        assert_eq!(policy.remote.len(), 2);
        assert_eq!(policy.cid_references, ["hero@x"]);
    }

    #[test]
    fn a_dropped_declaration_forgets_its_urls() {
        let mut policy = UrlPolicy::new(&SanitizeOptions::default());
        assert_eq!(
            sanitize_declarations(
                "background: url(https://t.example/a.png) expression(1)",
                &mut policy
            ),
            ""
        );
        assert!(policy.remote.is_empty());
    }

    #[test]
    fn comments_cannot_join_tokens() {
        assert_eq!(
            declarations("background: u/**/rl(https://t.example/a.png)"),
            "background:u rl(https://t.example/a.png);"
        );
    }

    #[test]
    fn stylesheets_keep_rules_media_and_supports_only() {
        assert_eq!(
            stylesheet(concat!(
                "@charset \"utf-8\"; @import url(https://t.example/a.css);",
                "@font-face { font-family: X; src: url(https://t.example/x.woff) }",
                "<!-- .a { color: red } -->",
                "@media only screen and (max-width: 600px) { td.stack { display: block !important; width: 100% } }",
                "@supports (display: grid) { .g { display: grid } }",
                "@keyframes spin { from { opacity: 0 } }",
                "@namespace svg url(http://www.w3.org/2000/svg);",
                "body > table, .x[data-a=\"b\"] { margin: 0 auto; position: fixed }",
                "@media url(https://t.example/q) { .q { color: red } }",
                ".empty { }",
            )),
            concat!(
                ".a{color:red;}",
                "@media only screen and (max-width: 600px){td.stack{display:block !important;width:100%;}}",
                "@supports (display: grid){.g{display:grid;}}",
                "body > table, .x[data-a=\"b\"]{margin:0 auto;}",
            )
        );
    }

    #[test]
    fn a_stylesheet_cannot_close_its_element() {
        let css = stylesheet(
            ".a { font-family: \"</style><script>alert(1)</script>\" } .\\3c /style\\3e { color: red }",
        );
        assert_eq!(
            css,
            ".a{font-family:\"\\3c /style>\\3c script>alert(1)\\3c /script>\";}"
        );
    }

    #[test]
    fn deep_nesting_is_dropped_not_overflowed() {
        let deep = format!("width: calc{}1px{}", "(".repeat(5_000), ")".repeat(5_000));
        assert_eq!(declarations(&deep), "");
        let rules = format!(
            "{}.a{{color:red}}{}",
            "@media screen {".repeat(1_000),
            "}".repeat(1_000)
        );
        assert_eq!(stylesheet(&rules), "");
    }
}
