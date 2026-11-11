//! Sender identity analysis for phishing and lookalike-domain warnings
//! (issue #91).
//!
//! The reader displays sender display names as-is, but display names are
//! untrusted: a lookalike domain (`paypa1.com` for `paypal.com`), a
//! display name that shows another address, a domain written with
//! letters from other alphabets, or a mismatched Reply-To is not
//! otherwise surfaced. This module flags them, information-first and
//! never blocking, per the security posture in
//! `docs/phase5-hardening.md`.
//!
//! Addresses are read with [`zmail_core::participants`], the parser the
//! reader shows senders with, so the domain checked here is always the
//! domain of the address the user sees.

use url::Host;
use zmail_core::participants::{Participant, parse_address_list};

use crate::links::{looks_like_domain, normalized_host, shown_hosts, visible_text};

/// How serious a sender-identity finding is, lowest first. The reader
/// shows one banner per message, at the highest tier that applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SafetyTier {
    /// Worth knowing, rarely a problem: replies go to another part of the
    /// sender's own domain (`news.example.com` for `example.com`), or the
    /// sender's domain is written with international letters.
    Info,
    /// Check before replying: replies go to an unrelated domain, the
    /// sender's name shows another address, or the sender's domain mixes
    /// alphabets.
    Caution,
    /// Likely impersonation: the From domain imitates a well-known one.
    Danger,
}

/// A warning about the sender identity of a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SenderSafetyReport {
    /// The From domain, lowercased, in its ASCII form (`xn--…` for an
    /// international name), when the From header has one.
    pub from_domain: Option<String>,
    /// The From domain looks like a well-known domain but is not it.
    pub lookalike_of: Option<String>,
    /// The Reply-To addresses point at a different domain than From.
    pub reply_to_mismatch: Option<String>,
    /// The sender's display name shows an address or website on a domain
    /// unrelated to the From domain (`"ceo@corp.example"
    /// <a@evil.example>`): that domain, in its ASCII form.
    pub name_shows: Option<String>,
    /// The display name is a mailing list's rewrite of a member's message
    /// (`'alice@example.com' via Dev Team`, Google Groups and Mailman)
    /// sent from the list's own address, and the member's address is on
    /// a domain unrelated to the list's: that address, lowercased. Only
    /// information, because that is how lists send; but the form is easy
    /// to forge, so it is still shown.
    pub mailing_list_member: Option<String>,
    /// The From domain is written with letters outside ASCII.
    pub international_domain: Option<InternationalDomain>,
}

/// A From domain written with letters outside ASCII (an internationalised
/// domain name, whether the header has it in Unicode or as `xn--`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InternationalDomain {
    /// The alphabets its letters come from, in order of first use
    /// ("Latin", "Cyrillic").
    pub scripts: Vec<&'static str>,
    /// Whether one of its labels mixes alphabets that are not normally
    /// written together (Latin with Cyrillic or Greek, say), the usual
    /// way to imitate another domain. Latin with Chinese, Japanese or
    /// Korean is not mixing.
    pub mixed: bool,
}

impl SenderSafetyReport {
    /// Whether any warning applies.
    pub fn is_safe(&self) -> bool {
        self.tier().is_none()
    }

    /// Whether the Reply-To domain is a parent or subdomain of the From
    /// domain.
    fn reply_to_related(&self) -> bool {
        match (
            self.from_domain.as_deref(),
            self.reply_to_mismatch.as_deref(),
        ) {
            (Some(from), Some(reply_to)) => domains_related(from, reply_to),
            _ => false,
        }
    }

    /// The highest tier that applies, or `None` when the sender looks
    /// safe. A lookalike From domain is [`SafetyTier::Danger`]; a Reply-To
    /// on an unrelated domain, a display name showing another domain and a
    /// From domain mixing alphabets are [`SafetyTier::Caution`]; a mailing
    /// list's rewrite of a member's address in the name, a Reply-To on a
    /// parent or subdomain of the From domain and an international From
    /// domain are [`SafetyTier::Info`].
    pub fn tier(&self) -> Option<SafetyTier> {
        if self.lookalike_of.is_some() {
            return Some(SafetyTier::Danger);
        }
        let mixed = self
            .international_domain
            .as_ref()
            .is_some_and(|domain| domain.mixed);
        let reply_to_unrelated = self.reply_to_mismatch.is_some() && !self.reply_to_related();
        if self.name_shows.is_some() || mixed || reply_to_unrelated {
            return Some(SafetyTier::Caution);
        }
        (self.mailing_list_member.is_some()
            || self.reply_to_mismatch.is_some()
            || self.international_domain.is_some())
        .then_some(SafetyTier::Info)
    }

    /// Human-readable warning lines, in priority order.
    pub fn warnings(&self) -> Vec<String> {
        let from_domain = self.from_domain.as_deref().unwrap_or_default();
        let mut warnings = Vec::new();
        if let Some(known) = &self.lookalike_of {
            warnings.push(format!(
                "The sender domain looks like {known} but is not {known}. Check the address carefully before replying."
            ));
        }
        if let Some(shown) = &self.name_shows {
            warnings.push(format!(
                "The sender's name shows {shown}, but the message comes from {from_domain}."
            ));
        }
        if let Some(member) = &self.mailing_list_member {
            warnings.push(format!(
                "Sent through a mailing list: the name shows {member}, the message came from {from_domain}."
            ));
        }
        if let Some(domain) = &self.international_domain {
            warnings.push(if domain.mixed {
                format!(
                    "The sender domain mixes letters from different alphabets ({}). Its plain form is {from_domain}.",
                    domain.scripts.join(", ")
                )
            } else {
                format!("The sender domain uses international letters. Its plain form is {from_domain}.")
            });
        }
        if let Some(domain) = &self.reply_to_mismatch {
            warnings.push(format!(
                "Reply-To points at {domain}, which differs from the From domain. Replies will go there."
            ));
        }
        warnings
    }
}

/// Whether one domain is the other or a subdomain of it, in either
/// direction (`mail.example.com` and `example.com`). Sibling subdomains
/// (`a.example.com`, `b.example.com`) do not count: without a public
/// suffix list `example.co.uk` and `evil.co.uk` would look related too.
fn domains_related(first: &str, second: &str) -> bool {
    let is_within = |inner: &str, outer: &str| {
        inner == outer
            || inner
                .strip_suffix(outer)
                .is_some_and(|prefix| prefix.ends_with('.'))
    };
    is_within(first, second) || is_within(second, first)
}

/// Whether a domain shown in a display name plausibly belongs to the
/// sender: related by [`domains_related`], or the same name under another
/// short ending (`amazon.com` in the name, `amazon.co.uk` sending), which
/// is how organisations send from their country domains. A sender
/// claiming `bank.example` from `bank.example.evil.example` is not.
fn same_organisation(shown: &str, from: &str) -> bool {
    /// The label in front of up to two short (one to three letters)
    /// endings: `amazon` for `amazon.co.uk` and `mail.amazon.com`.
    fn name(domain: &str) -> Option<&str> {
        let mut labels: Vec<&str> = domain.split('.').collect();
        for _ in 0..2 {
            if labels.len() > 1 && labels.last().is_some_and(|label| label.len() <= 3) {
                labels.pop();
            }
        }
        // `paypal.com.evil.example` has no short ending to drop: no name.
        if labels.len() == domain.split('.').count() {
            return None;
        }
        labels.last().copied()
    }
    domains_related(shown, from) || name(shown).is_some_and(|shown| name(from) == Some(shown))
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

/// Whether `domain` is a well-known domain or a subdomain of one.
fn is_known_domain(domain: &str) -> bool {
    KNOWN_DOMAINS.iter().any(|known| {
        domain == *known
            || domain
                .strip_suffix(known)
                .is_some_and(|prefix| prefix.ends_with('.'))
    })
}

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

/// The Latin letter a Cyrillic, Greek, Armenian or accented Latin letter
/// looks like, so `аpple.com` with a Cyrillic `а` reads as `apple.com`.
/// Other characters are kept.
fn latin_lookalike(ch: char) -> char {
    match ch {
        'а' | 'ɑ' | 'α' | 'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => 'a',
        'ь' => 'b',
        'с' | 'ϲ' | 'ç' | 'ć' | 'č' => 'c',
        'ԁ' | 'ď' | 'đ' => 'd',
        'е' | 'ё' | 'ε' | 'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ė' | 'ę' | 'ě' => 'e',
        'ɡ' | 'ğ' | 'ģ' => 'g',
        'һ' | 'հ' => 'h',
        'і' | 'ї' | 'ι' | 'ì' | 'í' | 'î' | 'ï' | 'ı' | 'ī' | 'į' => 'i',
        'ј' | 'ϳ' => 'j',
        'κ' | 'к' | 'ķ' => 'k',
        'ӏ' | 'ł' | 'ļ' | 'ľ' => 'l',
        'ո' | 'η' | 'ñ' | 'ń' | 'ň' | 'ņ' => 'n',
        'о' | 'ο' | 'օ' | 'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ő' => 'o',
        'р' | 'ρ' => 'p',
        'ԛ' | 'զ' => 'q',
        'ѕ' | 'ś' | 'š' | 'ş' => 's',
        'τ' | 'ť' | 'ţ' => 't',
        'υ' | 'ս' | 'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' | 'ű' | 'ų' => 'u',
        'ν' => 'v',
        'ԝ' | 'ω' => 'w',
        'х' | 'χ' => 'x',
        'у' | 'ý' | 'ÿ' => 'y',
        'ź' | 'ż' | 'ž' => 'z',
        ch => ch,
    }
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

/// The domain of one address, in the two forms the checks need.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AddressDomain {
    /// Lowercase ASCII (`xn--` labels for international ones), without a
    /// trailing dot: what is compared and shown.
    ascii: String,
    /// The same domain in Unicode, for the letter checks.
    unicode: String,
}

/// The domain of an address as the reader shows it (`user@example.com`,
/// or `user@example.com (comment)` when the header had one): what follows
/// the last `@`, up to a space or bracket, without a trailing dot.
fn address_domain_parts(address: &str) -> Option<AddressDomain> {
    let (_, domain) = address.rsplit_once('@')?;
    let domain = domain
        .split(|ch: char| {
            ch.is_whitespace() || matches!(ch, '<' | '>' | '(' | ')' | ',' | ';' | '"')
        })
        .next()
        .unwrap_or_default()
        .trim_end_matches('.');
    if domain.is_empty() {
        return None;
    }
    let lower = domain.to_lowercase();
    Some(match Host::parse(&lower) {
        Ok(host) => {
            let ascii = normalized_host(&host.to_string());
            let unicode = ascii
                .split('.')
                .map(|label| {
                    label
                        .strip_prefix("xn--")
                        .and_then(punycode::decode)
                        .unwrap_or_else(|| label.to_owned())
                })
                .collect::<Vec<_>>()
                .join(".");
            AddressDomain { ascii, unicode }
        }
        // Not a valid domain name: compared as written, without the
        // characters nobody can see.
        Err(_) => {
            let visible = visible_text(&lower);
            AddressDomain {
                ascii: visible.clone(),
                unicode: visible,
            }
        }
    })
}

/// Extracts the bare domain from an address or address list, lowercased
/// and in its ASCII form.
///
/// Accepts `user@example.com`, `Name <user@example.com>` (whatever the
/// name holds: commas, quotes, `<`), groups and comma-separated lists;
/// returns the domain of the first address that has one. Addresses are
/// read with [`zmail_core::participants::parse_address_list`], as the
/// reader shows them.
pub fn address_domain(address: &str) -> Option<String> {
    parse_address_list(address)
        .iter()
        .filter_map(|participant| participant.email.as_deref())
        .find_map(address_domain_parts)
        .map(|domain| domain.ascii)
}

/// Analyzes a message's sender identity for phishing indicators.
///
/// `from` is the stored sender (one mailbox, as the reader shows it);
/// `reply_to` is the raw Reply-To header value, if any. Both may carry
/// display names; Reply-To may be a list.
pub fn analyze_sender_identity(from: &str, reply_to: Option<&str>) -> SenderSafetyReport {
    let sender = Participant::parse(from);
    let domain = sender.email.as_deref().and_then(address_domain_parts);

    let lookalike_of = domain.as_ref().and_then(lookalike_known_domain);
    let international_domain = domain.as_ref().and_then(international_domain);
    let (name_shows, mailing_list_member) =
        match (&domain, sender.name.as_deref(), sender.email.as_deref()) {
            (Some(domain), Some(name), Some(email)) => {
                let name = visible_text(name);
                let member = list_member(&name, email, &domain.ascii);
                let shows = name_shows_domain(&name, member.as_deref(), &domain.ascii);
                let member = member.filter(|member| {
                    address_domain_parts(member).is_some_and(|member| {
                        looks_like_domain(&member.ascii)
                            && !same_organisation(&member.ascii, &domain.ascii)
                    })
                });
                (shows, member)
            }
            _ => (None, None),
        };
    let reply_to_mismatch = match (&domain, reply_to) {
        (Some(domain), Some(reply_to)) => reply_to_mismatch(reply_to, &domain.ascii),
        _ => None,
    };

    SenderSafetyReport {
        from_domain: domain.map(|domain| domain.ascii),
        lookalike_of,
        reply_to_mismatch,
        name_shows,
        mailing_list_member,
        international_domain,
    }
}

/// The Reply-To domain that differs from `from_domain`: the first one on
/// an unrelated domain, else the first on a parent or subdomain of it.
fn reply_to_mismatch(reply_to: &str, from_domain: &str) -> Option<String> {
    let differing: Vec<String> = parse_address_list(reply_to)
        .iter()
        .filter_map(|participant| participant.email.as_deref())
        .filter_map(address_domain_parts)
        .map(|domain| domain.ascii)
        .filter(|domain| domain != from_domain)
        .collect();
    differing
        .iter()
        .find(|domain| !domains_related(from_domain, domain))
        .or(differing.first())
        .cloned()
}

/// The domain a display name shows that does not belong to the sender: an
/// e-mail address in it (`"ceo@corp.example"`), a web address
/// (`www.bank.example`, `https://bank.example`), or a well-known domain
/// written bare (`PayPal.com`). Other dotted words ("Inc.", "J.R.R.",
/// "Mr.Smith") are not read as domains. `name` has been through
/// [`visible_text`]. `member`, the member's address in a mailing list's
/// rewrite of the name ([`list_member`]), is reported on its own, at a
/// lower tier.
fn name_shows_domain(name: &str, member: Option<&str>, from_domain: &str) -> Option<String> {
    let addresses = name
        .split(|ch: char| {
            ch.is_whitespace()
                || matches!(
                    ch,
                    '<' | '>' | '(' | ')' | '[' | ']' | '"' | '\'' | ',' | ';'
                )
        })
        .filter(|token| token.contains('@'))
        .filter(|token| member != Some(token.to_lowercase().as_str()))
        .filter_map(address_domain_parts)
        .map(|domain| domain.ascii)
        .filter(|domain| looks_like_domain(domain));
    let websites = shown_hosts(name)
        .into_iter()
        .filter(|shown| shown.written_as_address || is_known_domain(&shown.host))
        .map(|shown| shown.host);
    addresses
        .chain(websites)
        .find(|shown| !same_organisation(shown, from_domain))
}

/// The member's address, lowercased, when `name` is how a mailing list
/// rewrites a member's message (DMARC munging by Google Groups and
/// Mailman): `'alice@example.com' via Dev Team` or `alice@example.com via
/// Dev Team`, one address and nothing else before "via", sent from the
/// list's own address. That is an address whose local part is the list's
/// name (`dev-team@`, letters and digits compared), or one on a list host
/// (Google Groups, groups.io, a `lists.` or `groups.` domain). `None`
/// otherwise, so a name that just shows someone else's address, or says
/// "via" a list it is not sent from, is flagged as usual. The sender can
/// forge this form, which is why it lowers the warning to Info rather
/// than removing it.
fn list_member(name: &str, from_email: &str, from_domain: &str) -> Option<String> {
    let lower = name.to_lowercase();
    let (member, list) = lower.split_once(" via ")?;
    let member = member
        .trim()
        .trim_matches(|ch| matches!(ch, '\'' | '"'))
        .trim();
    let list = list.trim();
    if member.is_empty() || member.contains(char::is_whitespace) || !member.contains('@') {
        return None;
    }
    let letters =
        |text: &str| -> String { text.chars().filter(|ch| ch.is_alphanumeric()).collect() };
    let local = from_email
        .rsplit_once('@')
        .map(|(local, _)| local.to_lowercase())
        .unwrap_or_default();
    let names_the_list = !letters(list).is_empty() && letters(list) == letters(&local);
    let list_host = matches!(from_domain, "googlegroups.com" | "groups.io")
        || from_domain.ends_with(".groups.io")
        || from_domain.starts_with("lists.")
        || from_domain.starts_with("groups.");
    (names_the_list || list_host).then(|| member.to_owned())
}

/// Returns the well-known domain a candidate domain is impersonating, if
/// any. An exact match (including a confusable-character fold of the
/// candidate, Cyrillic and Greek lookalike letters included, against the
/// known domain) counts as impersonation because the byte-for-byte domain
/// differs; a one-character edit within the same domain family also
/// counts.
fn lookalike_known_domain(domain: &AddressDomain) -> Option<String> {
    if KNOWN_DOMAINS.contains(&domain.ascii.as_str()) {
        return None;
    }
    let latin: String = domain.unicode.chars().map(latin_lookalike).collect();
    let folded = fold_confusables(&latin);
    KNOWN_DOMAINS
        .iter()
        .find(|known| folded == **known || edit_distance_within(&folded, known, 1))
        .map(|known| (*known).to_owned())
}

/// The alphabets of an international domain's letters, and whether a
/// label mixes them; `None` for an all-ASCII domain.
fn international_domain(domain: &AddressDomain) -> Option<InternationalDomain> {
    if domain.unicode.is_ascii() {
        return None;
    }
    let mut scripts = Vec::new();
    let mut mixed = false;
    for label in domain.unicode.split('.') {
        let mut label_scripts: Vec<Script> = Vec::new();
        for script in label.chars().filter_map(Script::of) {
            if !label_scripts.contains(&script) {
                label_scripts.push(script);
            }
            if !scripts.contains(&script.name()) {
                scripts.push(script.name());
            }
        }
        mixed |= mixes_scripts(&label_scripts);
    }
    Some(InternationalDomain { scripts, mixed })
}

/// Whether letters from `scripts` in one label are unusual together.
/// Latin goes with Chinese, Japanese and Korean (as UTS #39's "highly
/// restrictive" level allows); any other pair is mixing.
fn mixes_scripts(scripts: &[Script]) -> bool {
    let without_latin_with_cjk = if scripts.contains(&Script::Cjk) {
        scripts
            .iter()
            .filter(|script| **script != Script::Latin)
            .count()
    } else {
        scripts.len()
    };
    without_latin_with_cjk > 1
}

/// The alphabet a letter belongs to, coarsely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Script {
    Latin,
    Greek,
    Cyrillic,
    Armenian,
    Hebrew,
    Arabic,
    Cherokee,
    /// Han, Hiragana, Katakana, Hangul and Bopomofo, which are written
    /// together.
    Cjk,
    /// Another script.
    Other,
}

impl Script {
    /// The script of a letter; `None` for digits, hyphens, combining
    /// marks and other characters any script uses.
    fn of(ch: char) -> Option<Self> {
        Some(match ch {
            'a'..='z' | 'A'..='Z' => Self::Latin,
            _ if ch.is_ascii() || !ch.is_alphabetic() => return None,
            '\u{00C0}'..='\u{024F}' | '\u{1E00}'..='\u{1EFF}' | '\u{0250}'..='\u{02AF}' => {
                Self::Latin
            }
            '\u{0370}'..='\u{03FF}' | '\u{1F00}'..='\u{1FFF}' => Self::Greek,
            '\u{0400}'..='\u{052F}'
            | '\u{1C80}'..='\u{1C8F}'
            | '\u{2DE0}'..='\u{2DFF}'
            | '\u{A640}'..='\u{A69F}' => Self::Cyrillic,
            '\u{0530}'..='\u{058F}' => Self::Armenian,
            '\u{0590}'..='\u{05FF}' => Self::Hebrew,
            '\u{0600}'..='\u{06FF}' | '\u{0750}'..='\u{077F}' | '\u{08A0}'..='\u{08FF}' => {
                Self::Arabic
            }
            '\u{13A0}'..='\u{13FF}' | '\u{AB70}'..='\u{ABBF}' => Self::Cherokee,
            '\u{1100}'..='\u{11FF}'
            | '\u{3040}'..='\u{30FF}'
            | '\u{3100}'..='\u{312F}'
            | '\u{3130}'..='\u{318F}'
            | '\u{3400}'..='\u{4DBF}'
            | '\u{4E00}'..='\u{9FFF}'
            | '\u{AC00}'..='\u{D7AF}'
            | '\u{F900}'..='\u{FAFF}'
            | '\u{20000}'..='\u{3FFFF}' => Self::Cjk,
            _ => Self::Other,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Self::Latin => "Latin",
            Self::Greek => "Greek",
            Self::Cyrillic => "Cyrillic",
            Self::Armenian => "Armenian",
            Self::Hebrew => "Hebrew",
            Self::Arabic => "Arabic",
            Self::Cherokee => "Cherokee",
            Self::Cjk => "Chinese, Japanese or Korean",
            Self::Other => "another alphabet",
        }
    }
}

/// Punycode (RFC 3492) decoding, for the `xn--` labels of international
/// domain names. The `url` crate only encodes.
mod punycode {
    const BASE: u32 = 36;
    const T_MIN: u32 = 1;
    const T_MAX: u32 = 26;
    const SKEW: u32 = 38;
    const DAMP: u32 = 700;
    const INITIAL_BIAS: u32 = 72;
    const INITIAL_N: u32 = 128;
    /// Longer labels are not valid DNS labels; this bounds the work.
    const MAX_LABEL: usize = 63;

    fn adapt(delta: u32, points: u32, first: bool) -> u32 {
        let mut delta = if first { delta / DAMP } else { delta / 2 };
        delta += delta / points;
        let mut k = 0;
        while delta > ((BASE - T_MIN) * T_MAX) / 2 {
            delta /= BASE - T_MIN;
            k += BASE;
        }
        k + (BASE - T_MIN + 1) * delta / (delta + SKEW)
    }

    fn digit(byte: u8) -> Option<u32> {
        match byte {
            b'a'..=b'z' => Some(u32::from(byte - b'a')),
            b'A'..=b'Z' => Some(u32::from(byte - b'A')),
            b'0'..=b'9' => Some(u32::from(byte - b'0') + 26),
            _ => None,
        }
    }

    /// The Unicode text a punycode label (without its `xn--`) encodes, or
    /// `None` when it is not valid punycode.
    pub(super) fn decode(input: &str) -> Option<String> {
        if !input.is_ascii() || input.len() > MAX_LABEL {
            return None;
        }
        let (basic, extended) = match input.rfind('-') {
            Some(at) => (&input[..at], &input[at + 1..]),
            None => ("", input),
        };
        let mut output: Vec<char> = basic.chars().collect();
        let mut n = INITIAL_N;
        let mut i: u32 = 0;
        let mut bias = INITIAL_BIAS;
        let mut bytes = extended.bytes().peekable();
        while bytes.peek().is_some() {
            let old_i = i;
            let mut weight: u32 = 1;
            let mut k = BASE;
            loop {
                let digit = digit(bytes.next()?)?;
                i = i.checked_add(digit.checked_mul(weight)?)?;
                let threshold = if k <= bias {
                    T_MIN
                } else if k >= bias + T_MAX {
                    T_MAX
                } else {
                    k - bias
                };
                if digit < threshold {
                    break;
                }
                weight = weight.checked_mul(BASE - threshold)?;
                k += BASE;
            }
            let points = u32::try_from(output.len()).ok()? + 1;
            bias = adapt(i - old_i, points, old_i == 0);
            n = n.checked_add(i / points)?;
            i %= points;
            output.insert(usize::try_from(i).ok()?, char::from_u32(n)?);
            i += 1;
        }
        Some(output.into_iter().collect())
    }
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
    fn awkward_display_names_do_not_hide_the_sender_domain() {
        for from in [
            // A quoted comma in the display name.
            "\"PayPal, Inc.\" <support@paypa1.com>",
            "\"PayPal, Inc., Security\" <support@paypa1.com>",
            // An unquoted one, as some adapters store it.
            "PayPal, Inc. <support@paypa1.com>",
            // One or more `<` in the display name.
            "a<b> <support@paypa1.com>",
            "\"a<b<c\" <support@paypa1.com>",
            "<<x> <support@paypa1.com>",
            // Trailing junk after the address.
            "PayPal <support@paypa1.com> (Security)",
            "support@paypa1.com (PayPal)",
            "PayPal <support@paypa1.com.>",
            // A group.
            "PayPal: \"PayPal, Inc.\" <support@paypa1.com>;",
        ] {
            let report = analyze_sender_identity(from, Some("help@evil.example"));
            assert_eq!(report.from_domain.as_deref(), Some("paypa1.com"), "{from}");
            assert_eq!(report.lookalike_of.as_deref(), Some("paypal.com"), "{from}");
            assert_eq!(
                report.reply_to_mismatch.as_deref(),
                Some("evil.example"),
                "{from}"
            );
        }
    }

    #[test]
    fn the_domain_checked_is_the_address_the_reader_shows() {
        for from in [
            "\"PayPal, Inc.\" <support@paypa1.com>",
            "a<b> <support@paypa1.com>",
            "Name <good@example.com> <support@paypa1.com>",
            "\"service@paypal.com\" <support@paypa1.com>",
        ] {
            let (_, shown) = zmail_core::participants::sender_identity(from);
            let shown = shown.expect("the reader shows an address");
            assert!(shown.ends_with("@paypa1.com"), "{from}: {shown}");
            assert_eq!(
                analyze_sender_identity(from, None).from_domain.as_deref(),
                Some("paypa1.com"),
                "{from}"
            );
        }
    }

    #[test]
    fn reply_to_lists_with_awkward_names_are_read_whole() {
        for reply_to in [
            "\"Billing, Accounts\" <collect@evil.example>",
            "Billing, Accounts <collect@evil.example>",
            "a<b> <collect@evil.example>",
            "Team: \"Doe, Jane\" <collect@evil.example>;",
            // A same-domain address first does not hide the other one.
            "billing@paypal.com, collect@evil.example",
        ] {
            let report = analyze_sender_identity("Billing <billing@paypal.com>", Some(reply_to));
            assert_eq!(
                report.reply_to_mismatch.as_deref(),
                Some("evil.example"),
                "{reply_to}"
            );
            assert_eq!(report.tier(), Some(SafetyTier::Caution), "{reply_to}");
        }
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
    fn tiers_rank_impersonation_above_reply_to_mismatches() {
        let safe = analyze_sender_identity("news@lwn.net", Some("news@lwn.net"));
        assert_eq!(safe.tier(), None);

        let lookalike = analyze_sender_identity("security@paypa1.com", None);
        assert_eq!(lookalike.tier(), Some(SafetyTier::Danger));
        assert_eq!(lookalike.from_domain.as_deref(), Some("paypa1.com"));

        let unrelated = analyze_sender_identity("billing@paypal.com", Some("evil@evil.example"));
        assert_eq!(unrelated.tier(), Some(SafetyTier::Caution));

        // A Reply-To on a subdomain (or the parent) of the From domain is
        // still shown, but only as information.
        let subdomain = analyze_sender_identity(
            "GitHub <noreply@github.com>",
            Some("reply@reply.github.com"),
        );
        assert_eq!(subdomain.tier(), Some(SafetyTier::Info));
        let parent = analyze_sender_identity("news@mail.example.com", Some("team@example.com"));
        assert_eq!(parent.tier(), Some(SafetyTier::Info));

        // Look-alike suffixes and siblings are not related.
        let suffix = analyze_sender_identity("a@example.com", Some("b@notexample.com"));
        assert_eq!(suffix.tier(), Some(SafetyTier::Caution));
        let sibling = analyze_sender_identity("a@one.example.com", Some("b@two.example.com"));
        assert_eq!(sibling.tier(), Some(SafetyTier::Caution));

        // Impersonation wins when both apply.
        let both = analyze_sender_identity("security@paypa1.com", Some("evil@evil.example"));
        assert_eq!(both.tier(), Some(SafetyTier::Danger));
        assert!(SafetyTier::Info < SafetyTier::Caution);
        assert!(SafetyTier::Caution < SafetyTier::Danger);
    }

    #[test]
    fn both_warnings_can_apply_at_once() {
        let report =
            analyze_sender_identity("Security <security@paypa1.com>", Some("evil@evil.example"));
        assert_eq!(report.lookalike_of, Some("paypal.com".to_owned()));
        assert_eq!(report.reply_to_mismatch, Some("evil.example".to_owned()));
        assert_eq!(report.warnings().len(), 2);
    }

    #[test]
    fn display_names_showing_another_address_are_caution() {
        for (from, shown) in [
            ("\"ceo@corp.example\" <a@evil.example>", "corp.example"),
            ("CEO (ceo@corp.example) <a@evil.example>", "corp.example"),
            (
                "\"Support <help@bank.example>\" <x@evil.example>",
                "bank.example",
            ),
            ("Bank www.bank.example <x@evil.example>", "www.bank.example"),
            (
                "https://bank.example/login <x@evil.example>",
                "bank.example",
            ),
            // A well-known domain written bare.
            ("PayPal.com <x@random.example>", "paypal.com"),
            (
                "Service paypal\u{200B}.com <x@random.example>",
                "paypal.com",
            ),
        ] {
            let report = analyze_sender_identity(from, None);
            assert_eq!(report.name_shows.as_deref(), Some(shown), "{from}");
            assert_eq!(report.tier(), Some(SafetyTier::Caution), "{from}");
            assert!(!report.is_safe());
        }
    }

    #[test]
    fn mailing_list_rewrites_of_a_member_address_are_info() {
        // Google Groups and Mailman put the member's address in the name
        // and send from the list's own address.
        for from in [
            "'alice@example.com' via Dev Team <dev-team@googlegroups.com>",
            "\"'alice@example.com' via Dev Team\" <dev-team@googlegroups.com>",
            "alice@example.com via Announcements <announce@googlegroups.com>",
            "'alice@example.com' via Mailman-Users <mailman-users@python.org>",
            "alice@example.com via Dev <dev@lists.debian.org>",
            "'Alice@Example.com' VIA Rust Users <rust-users@groups.io>",
        ] {
            let report = analyze_sender_identity(from, None);
            assert_eq!(report.name_shows, None, "{from}");
            assert_eq!(
                report.mailing_list_member.as_deref(),
                Some("alice@example.com"),
                "{from}"
            );
            // Still shown, as information.
            assert_eq!(report.tier(), Some(SafetyTier::Info), "{from}");
            assert!(!report.is_safe());
        }
        // The form can be forged; it is still shown, as information.
        let forged = analyze_sender_identity(
            "'ceo@corp.example' via Payments <payments@evil.example>",
            None,
        );
        assert_eq!(
            forged.mailing_list_member.as_deref(),
            Some("ceo@corp.example")
        );
        assert_eq!(forged.tier(), Some(SafetyTier::Info));
        // A member on the list's own domain is nothing to report.
        let report = analyze_sender_identity(
            "'bob@python.org' via Python-Dev <python-dev@python.org>",
            None,
        );
        assert!(report.is_safe(), "{report:?}");
        // A list rewrite with an unrelated Reply-To is Caution, for the
        // Reply-To.
        let report = analyze_sender_identity(
            "'alice@example.com' via Dev <dev@googlegroups.com>",
            Some("x@evil.example"),
        );
        assert_eq!(report.tier(), Some(SafetyTier::Caution));
    }

    #[test]
    fn a_via_name_not_sent_from_its_list_still_warns() {
        for (from, shown) in [
            // Just someone else's address.
            ("\"ceo@corp.example\" <a@evil.example>", "corp.example"),
            ("'ceo@corp.example' <a@evil.example>", "corp.example"),
            // "via" a list whose address it is not sent from.
            (
                "'ceo@corp.example' via Payments <x@evil.example>",
                "corp.example",
            ),
            (
                "'ceo@corp.example' via Dev Team <payments@evil.example>",
                "corp.example",
            ),
            (
                "'ceo@corp.example' via <x@googlegroups.com>",
                "corp.example",
            ),
            // More than one address before "via".
            (
                "ceo@corp.example and cfo@corp.example via Dev <dev@googlegroups.com>",
                "corp.example",
            ),
        ] {
            let report = analyze_sender_identity(from, None);
            assert_eq!(report.name_shows.as_deref(), Some(shown), "{from}");
            assert_eq!(report.mailing_list_member, None, "{from}");
            assert_eq!(report.tier(), Some(SafetyTier::Caution), "{from}");
        }
    }

    #[test]
    fn display_names_naming_the_sender_itself_are_safe() {
        for from in [
            "\"support@example.com\" <support@example.com>",
            "\"help@example.com\" <noreply@mail.example.com>",
            "Example.com <noreply@example.com>",
            "Amazon.com <auto-confirm@amazon.co.uk>",
            "\"orders@amazon.com\" <orders@amazon.com.br>",
            "Booking.com <noreply@mailer.booking.com>",
            // Dotted words that are not domains.
            "Acme Inc. <billing@acme.example>",
            "J.R.R. Tolkien <jrr@tolkien.example>",
            "Mr.Smith <smith@example.com>",
            "Node.js Weekly <news@cooperpress.example>",
            "Bob @ Home <bob@example.com>",
        ] {
            let report = analyze_sender_identity(from, None);
            assert_eq!(report.name_shows, None, "{from}");
            assert!(report.is_safe(), "{from} produced {report:?}");
        }
        // The same name under a long ending is not the same organisation.
        let report = analyze_sender_identity(
            "\"service@paypal.com\" <service@paypal.com.evil.example>",
            None,
        );
        assert_eq!(report.name_shows.as_deref(), Some("paypal.com"));
    }

    #[test]
    fn punycode_labels_decode() {
        assert_eq!(punycode::decode("mnchen-3ya").as_deref(), Some("münchen"));
        assert_eq!(punycode::decode("bcher-kva").as_deref(), Some("bücher"));
        assert_eq!(
            punycode::decode("80ak6aa92e").as_deref(),
            Some("\u{0430}\u{0440}\u{0440}\u{04CF}\u{0435}")
        );
        for broken in ["", "a-!", "zzzzzzzzzzzzzzzzzzz", "ü", &"a".repeat(64)] {
            let _ = punycode::decode(broken);
        }
        assert_eq!(punycode::decode("99999999999"), None);
        // Whatever the URL parser encodes reads back.
        for unicode in [
            "\u{0430}pple.com",
            "日本語.example",
            "παράδειγμα.example",
            "bücher.example",
        ] {
            let ascii = Host::parse(unicode).expect("a valid name").to_string();
            assert!(ascii.contains("xn--"), "{ascii}");
            let parts = address_domain_parts(&format!("x@{ascii}")).expect("a domain");
            assert_eq!(parts.unicode, unicode);
            assert_eq!(parts.ascii, ascii);
        }
    }

    #[test]
    fn international_lookalikes_are_impersonation() {
        for from in [
            // A Cyrillic а, as Unicode and as punycode.
            "Apple <support@\u{0430}pple.com>",
            "support@xn--pple-43d.com",
            // Every letter Cyrillic.
            "Apple <id@xn--80ak6aa92e.com>",
            // Greek ο in google.
            "Google <x@g\u{03BF}\u{03BF}gle.com>",
            // An accented letter.
            "PayPal <x@payp\u{00E0}l.com>",
        ] {
            let report = analyze_sender_identity(from, None);
            assert!(report.lookalike_of.is_some(), "{from}: {report:?}");
            assert_eq!(report.tier(), Some(SafetyTier::Danger), "{from}");
            let domain = report.from_domain.expect("a domain");
            assert!(domain.is_ascii() && domain.contains("xn--"), "{domain}");
        }
    }

    #[test]
    fn international_domains_are_info_and_mixed_alphabets_caution() {
        // One alphabet: worth knowing, nothing more.
        for (from, scripts) in [
            ("Hans <hans@m\u{00FC}ller.example>", vec!["Latin"]),
            ("x@xn--mnchen-3ya.example", vec!["Latin"]),
            (
                "x@\u{043F}\u{0440}\u{0438}\u{043C}\u{0435}\u{0440}.\u{0440}\u{0444}",
                vec!["Cyrillic"],
            ),
            ("x@例え.テスト", vec!["Chinese, Japanese or Korean"]),
            // Latin with Japanese is normal.
            (
                "x@abc\u{65E5}\u{672C}.example",
                vec!["Latin", "Chinese, Japanese or Korean"],
            ),
        ] {
            let report = analyze_sender_identity(from, None);
            let domain = report.international_domain.clone().expect("international");
            assert!(!domain.mixed, "{from}");
            assert_eq!(domain.scripts, scripts, "{from}");
            assert_eq!(report.tier(), Some(SafetyTier::Info), "{from}");
        }
        // Latin and Cyrillic in one label, imitating nothing well known.
        let report = analyze_sender_identity("Bank <x@b\u{0430}nk.example>", None);
        assert_eq!(report.lookalike_of, None);
        let domain = report.international_domain.clone().expect("international");
        assert!(domain.mixed);
        assert_eq!(domain.scripts, vec!["Latin", "Cyrillic"]);
        assert_eq!(report.tier(), Some(SafetyTier::Caution));
        assert_eq!(
            report.from_domain,
            Some(
                Host::parse("b\u{0430}nk.example")
                    .expect("valid")
                    .to_string()
            )
        );
        // Different alphabets in different labels are not mixing.
        let report = analyze_sender_identity(
            "x@\u{043F}\u{0440}\u{0438}\u{043C}\u{0435}\u{0440}.example",
            None,
        );
        assert!(!report.international_domain.expect("international").mixed);
        // Plain ASCII domains are not international.
        assert_eq!(
            analyze_sender_identity("x@example.com", None).international_domain,
            None
        );
    }
}
