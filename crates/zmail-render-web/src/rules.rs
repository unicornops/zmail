//! The content blocker: the second wall behind the Content Security Policy.
//!
//! WebKit compiles a JSON rule list (a `WKContentRuleList`) and checks every
//! load against it in the network layer, before any request leaves the
//! machine, whatever the page or its CSP say. The list blocks everything,
//! then lets through Zmail's own schemes, `data:` images and, when the user
//! loaded remote content, images from each allowed origin over https.
//! WebKit's `url-filter` is a restricted regular expression (no
//! alternation), so each origin is its own rule.

use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
};

use crate::document::{CID_SCHEME, MAIL_SCHEME};

/// The rule list for a page that may load images from `allowed_origins`
/// (already checked with [`crate::document::public_https_origin`]).
pub fn content_rule_list(allowed_origins: &[String]) -> String {
    let mut rules = vec![
        r#"{"trigger":{"url-filter":".*"},"action":{"type":"block"}}"#.to_owned(),
        format!(
            r#"{{"trigger":{{"url-filter":"^{}:"}},"action":{{"type":"ignore-previous-rules"}}}}"#,
            regex_escape(MAIL_SCHEME)
        ),
        format!(
            r#"{{"trigger":{{"url-filter":"^{}:","resource-type":["image"]}},"action":{{"type":"ignore-previous-rules"}}}}"#,
            regex_escape(CID_SCHEME)
        ),
        r#"{"trigger":{"url-filter":"^data:","resource-type":["image"]},"action":{"type":"ignore-previous-rules"}}"#
            .to_owned(),
    ];
    for origin in allowed_origins {
        rules.push(format!(
            r#"{{"trigger":{{"url-filter":"^{}/","resource-type":["image"]}},"action":{{"type":"ignore-previous-rules"}}}}"#,
            json_escape(&regex_escape(origin))
        ));
    }
    format!("[{}]", rules.join(","))
}

/// A stable identifier for a rule list, so WebKit's store keeps one
/// compiled list per distinct set of origins.
pub fn rule_list_identifier(rule_list: &str) -> String {
    let mut hasher = DefaultHasher::new();
    rule_list.hash(&mut hasher);
    format!("zmail-mail-blocker-v1-{:016x}", hasher.finish())
}

/// Escapes the characters WebKit's `url-filter` syntax treats specially.
fn regex_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(
            ch,
            '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '^' | '$' | '\\'
        ) {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

/// Escapes a string for a JSON string literal.
fn json_escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn everything_is_blocked_except_zmail_schemes_and_data_images() {
        let list = content_rule_list(&[]);
        let rules: serde_json::Value = serde_json::from_str(&list).expect("valid JSON");
        let rules = rules.as_array().expect("an array");
        assert_eq!(rules.len(), 4);
        assert_eq!(rules[0]["trigger"]["url-filter"], ".*");
        assert_eq!(rules[0]["action"]["type"], "block");
        assert_eq!(rules[1]["trigger"]["url-filter"], "^zmail-mail:");
        assert_eq!(rules[2]["trigger"]["url-filter"], "^cid:");
        assert_eq!(rules[3]["trigger"]["url-filter"], "^data:");
        for rule in &rules[1..] {
            assert_eq!(rule["action"]["type"], "ignore-previous-rules");
        }
    }

    #[test]
    fn allowed_origins_are_exact_and_images_only() {
        let list = content_rule_list(&["https://img.example.com:8443".to_owned()]);
        let rules: serde_json::Value = serde_json::from_str(&list).expect("valid JSON");
        let origin = &rules[4];
        assert_eq!(
            origin["trigger"]["url-filter"],
            "^https://img\\.example\\.com:8443/"
        );
        assert_eq!(origin["trigger"]["resource-type"][0], "image");
    }

    #[test]
    fn identifiers_differ_per_origin_set() {
        let none = rule_list_identifier(&content_rule_list(&[]));
        let one = rule_list_identifier(&content_rule_list(&["https://a.example".to_owned()]));
        assert_ne!(none, one);
        assert_eq!(none, rule_list_identifier(&content_rule_list(&[])));
    }
}
