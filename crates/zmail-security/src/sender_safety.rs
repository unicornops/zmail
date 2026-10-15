//! Sender identity analysis for phishing and lookalike-domain warnings
//! (issue #91).
//!
//! The reader displays sender display names as-is, but display names are
//! untrusted: a lookalike domain (`paypa1.com` for `paypal.com`) or a
//! mismatched Reply-To is not otherwise surfaced. This module flags
//! both, information-first and never blocking, per the security posture
//! in `docs/phase5-hardening.md`.

/// A warning about the sender identity of a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SenderSafetyReport {
    /// The From domain looks like a well-known domain but is not it.
    pub lookalike_of: Option<String>,
    /// The Reply-To addresses point at a different domain than From.
    pub reply_to_mismatch: Option<String>,
}

impl SenderSafetyReport {
    /// Whether any warning applies.
    pub fn is_safe(&self) -> bool {
        self.lookalike_of.is_none() && self.reply_to_mismatch.is_none()
    }

    /// Human-readable warning lines for the reader, in priority order.
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        if let Some(known) = &self.lookalike_of {
            warnings.push(format!(
                "The sender domain looks like {known} but is not {known}. Check the address carefully before replying."
            ));
        }
        if let Some(domain) = &self.reply_to_mismatch {
            warnings.push(format!(
                "Reply-To points at {domain}, which differs from the From domain. Replies will go there."
            ));
        }
        warnings
    }
}

/// Well-known domains most often impersonated in phishing.
const KNOWN_DOMAINS: &[&str] = &[
    "amazon.com",
    "americanexpress.com",
    "apple.com",
    "bankofamerica.com",
    "binance.com",
    "chase.com",
    "coinbase.com",
    "dhl.com",
    "dropbox.com",
    "ebay.com",
    "facebook.com",
    "fedex.com",
    "github.com",
    "google.com",
    "icloud.com",
    "instagram.com",
    "linkedin.com",
    "live.com",
    "netflix.com",
    "office.com",
    "outlook.com",
    "paypal.com",
    "stripe.com",
    "ups.com",
    "wellsfargo.com",
    "whatsapp.com",
];

/// Confusable character fold: digits and lookalike letters collapse to
/// their letter equivalents so `paypa1` folds to `paypal`, `g00gle`
/// folds to `google`, and `rn` collapses to `m`.
fn fold_confusables(domain: &str) -> String {
    domain
        .replace("rn", "m")
        .chars()
        .map(|c| match c {
            '0' => 'o',
            '1' | '|' => 'l',
            '3' => 'e',
            '4' => 'a',
            '5' => 's',
            '6' | '8' => 'b',
            '7' => 't',
            '@' => 'a',
            _ => c,
        })
        .collect()
}

/// Minimal edit distance (Levenshtein) with an early exit above `max`.
fn edit_distance_within(left: &str, right: &str, max: usize) -> bool {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    if left.len().abs_diff(right.len()) > max {
        return false;
    }
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0usize; right.len() + 1];
    for (i, left_char) in left.iter().enumerate() {
        current[0] = i + 1;
        for (j, right_char) in right.iter().enumerate() {
            let cost = usize::from(left_char != right_char);
            current[j + 1] = (previous[j + 1] + 1)
                .min(current[j] + 1)
                .min(previous[j] + cost);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()] <= max
}

/// Extracts the bare domain from an address or address list.
///
/// Accepts `user@example.com`, `Name <user@example.com>`, and
/// comma-separated lists; returns the lowercased domain of the first
/// address that has one.
pub fn address_domain(address: &str) -> Option<String> {
    for candidate in address.split(',') {
        let candidate = candidate.trim();
        let inner = candidate
            .split('<')
            .nth(1)
            .map(|rest| rest.trim_end_matches('>').trim())
            .unwrap_or(candidate);
        let domain = inner.rsplit_once('@')?.1;
        let domain = domain.trim().to_ascii_lowercase();
        if !domain.is_empty() {
            return Some(domain);
        }
    }
    None
}

/// Analyzes a message's sender identity for phishing indicators.
///
/// `from` is the raw From header value; `reply_to` is the raw Reply-To
/// header value, if any. Both may carry display names and lists.
pub fn analyze_sender_identity(from: &str, reply_to: Option<&str>) -> SenderSafetyReport {
    let from_domain = address_domain(from);

    let lookalike_of = from_domain.as_deref().and_then(lookalike_known_domain);

    let reply_to_mismatch = match (&from_domain, reply_to) {
        (Some(from_domain), Some(reply_to)) if !reply_to.trim().is_empty() => {
            address_domain(reply_to).filter(|domain| domain != from_domain)
        }
        _ => None,
    };

    SenderSafetyReport {
        lookalike_of,
        reply_to_mismatch,
    }
}

/// Returns the well-known domain a candidate domain is impersonating, if
/// any. An exact match (including a confusable-character fold of the
/// candidate against the known domain) counts as impersonation because
/// the byte-for-byte domain differs; a one-character edit within the
/// same domain family also counts.
fn lookalike_known_domain(domain: &str) -> Option<String> {
    let folded = fold_confusables(&domain.to_ascii_lowercase());
    for known in KNOWN_DOMAINS {
        if domain == *known {
            return None;
        }
        if folded == **known || edit_distance_within(&folded, known, 1) {
            return Some((*known).to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_domains_from_display_forms() {
        assert_eq!(
            address_domain("user@example.com"),
            Some("example.com".to_owned())
        );
        assert_eq!(
            address_domain("Alice <alice@example.com>"),
            Some("example.com".to_owned())
        );
        assert_eq!(
            address_domain("One <one@a.com>, Two <two@b.com>"),
            Some("a.com".to_owned())
        );
        assert_eq!(address_domain("no-domain"), None);
    }

    #[test]
    fn flags_character_swap_lookalikes() {
        let report = analyze_sender_identity("Security <support@paypa1.com>", None);
        assert_eq!(report.lookalike_of, Some("paypal.com".to_owned()));
        assert!(report.reply_to_mismatch.is_none());
        assert!(!report.is_safe());
        assert_eq!(report.warnings().len(), 1);
    }

    #[test]
    fn flags_digit_substitution_lookalikes() {
        let report = analyze_sender_identity("no-reply@g00gle.com", None);
        assert_eq!(report.lookalike_of, Some("google.com".to_owned()));
    }

    #[test]
    fn flags_single_edit_lookalikes() {
        // paypal.com with one character inserted
        let report = analyze_sender_identity("security@payppal.com", None);
        assert_eq!(report.lookalike_of, Some("paypal.com".to_owned()));
        // apple.com with one character deleted
        let report = analyze_sender_identity("news@aple.com", None);
        assert_eq!(report.lookalike_of, Some("apple.com".to_owned()));
    }

    #[test]
    fn legitimate_known_domains_stay_safe() {
        for (address, reply_to) in [
            ("service@paypal.com", None),
            ("Google <no-reply@google.com>", None),
            ("GitHub <noreply@github.com>", Some("noreply@github.com")),
        ] {
            let report = analyze_sender_identity(address, reply_to);
            assert!(report.is_safe(), "{address} produced {report:?}");
        }
    }

    #[test]
    fn unrelated_domains_stay_safe() {
        let report = analyze_sender_identity("news@lwn.net", None);
        assert!(report.is_safe());
        // A domain containing a known name as a substring is not a
        // one-character edit of it.
        let report = analyze_sender_identity("news@paypal-forum.example", None);
        assert!(report.is_safe());
    }

    #[test]
    fn flags_reply_to_domain_mismatches() {
        let report = analyze_sender_identity(
            "Billing <billing@paypal.com>",
            Some("attacker <evil@evil.example>"),
        );
        assert_eq!(report.reply_to_mismatch, Some("evil.example".to_owned()));
        assert_eq!(report.lookalike_of, None);
        assert_eq!(report.warnings().len(), 1);
    }

    #[test]
    fn matching_reply_to_domains_stay_safe() {
        let report =
            analyze_sender_identity("Support <support@paypal.com>", Some("support@paypal.com"));
        assert!(report.is_safe());
    }

    #[test]
    fn both_warnings_can_apply_at_once() {
        let report =
            analyze_sender_identity("Security <security@paypa1.com>", Some("evil@evil.example"));
        assert_eq!(report.lookalike_of, Some("paypal.com".to_owned()));
        assert_eq!(report.reply_to_mismatch, Some("evil.example".to_owned()));
        assert_eq!(report.warnings().len(), 2);
    }
}
