//! Sends that wait for the user to confirm a recipient's new key.
//!
//! A Proton contact card can pin a recipient's public keys. When none of
//! the keys Proton serves for that recipient is a pinned one, Bridge sends
//! to the first served key anyway; Zmail stops instead (ADR 0004,
//! "Pinned keys that no longer match"): the protocol crate returns a
//! [`PinnedKeyChanged`] error, sync stores its message on the outbox job
//! (outbox errors are kept as plain text, as for
//! [`crate::reauth::ReauthRequired`]), and the shell reads it back with
//! [`PinnedKeyChanged::of_message`] to offer trusting the served key for
//! that recipient. Trusting records a [`KeyTrust`] in the local store; the
//! contact card on the server is never changed.

use std::fmt;

/// The fixed start of a stored [`PinnedKeyChanged`] message. Changing it
/// orphans messages already stored, which then read as ordinary errors.
const LEAD: &str = "The encryption key for ";
const PINNED: &str = " changed: the contact card pins ";
const SERVED: &str = "; Proton serves ";
const TAIL: &str = ". Trust the new key to send.";

/// A send stopped because the recipient's contact card pins keys and
/// Proton serves none of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedKeyChanged {
    /// The recipient's address.
    pub recipient: String,
    /// The fingerprints the contact card pins, upper-case hex.
    pub pinned_fingerprints: Vec<String>,
    /// The fingerprints of the keys Proton serves that mail could be
    /// encrypted to, upper-case hex; the first is the one a send uses.
    pub served_fingerprints: Vec<String>,
}

impl PinnedKeyChanged {
    /// The error for `recipient`, with the fingerprints in any case.
    pub fn new(
        recipient: impl Into<String>,
        pinned_fingerprints: impl IntoIterator<Item = String>,
        served_fingerprints: impl IntoIterator<Item = String>,
    ) -> Self {
        let upper = |fingerprints: Vec<String>| -> Vec<String> {
            fingerprints
                .into_iter()
                .map(|fingerprint| fingerprint.to_ascii_uppercase())
                .collect()
        };
        Self {
            recipient: recipient.into(),
            pinned_fingerprints: upper(pinned_fingerprints.into_iter().collect()),
            served_fingerprints: upper(served_fingerprints.into_iter().collect()),
        }
    }

    /// The served key a send would use, which trusting approves.
    pub fn new_fingerprint(&self) -> Option<&str> {
        self.served_fingerprints.first().map(String::as_str)
    }

    /// The error a stored outbox message records, if it is one.
    pub fn of_message(message: &str) -> Option<Self> {
        let rest = message.strip_prefix(LEAD)?.strip_suffix(TAIL)?;
        let (rest, served) = rest.rsplit_once(SERVED)?;
        let (recipient, pinned) = rest.rsplit_once(PINNED)?;
        let fingerprints = |list: &str| -> Option<Vec<String>> {
            let fingerprints: Vec<String> = list.split(", ").map(str::to_owned).collect();
            fingerprints
                .iter()
                .all(|fingerprint| {
                    !fingerprint.is_empty()
                        && fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
                .then_some(fingerprints)
        };
        (!recipient.is_empty()).then_some(())?;
        Some(Self::new(
            recipient,
            fingerprints(pinned)?,
            fingerprints(served)?,
        ))
    }
}

impl fmt::Display for PinnedKeyChanged {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{LEAD}{}{PINNED}{}{SERVED}{}{TAIL}",
            self.recipient,
            self.pinned_fingerprints.join(", "),
            self.served_fingerprints.join(", ")
        )
    }
}

impl std::error::Error for PinnedKeyChanged {}

/// The user's decision to send to a key Proton serves for a recipient
/// although the contact card pins other keys. It covers exactly this
/// address and this fingerprint, on one account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyTrust {
    /// The recipient's address.
    pub recipient: String,
    /// The trusted key's fingerprint.
    pub fingerprint: String,
}

impl KeyTrust {
    /// Whether this trusts `fingerprint` for `recipient` (addresses and
    /// fingerprints compare without regard to case).
    pub fn covers(&self, recipient: &str, fingerprint: &str) -> bool {
        self.recipient.eq_ignore_ascii_case(recipient.trim())
            && self.fingerprint.eq_ignore_ascii_case(fingerprint)
    }
}

/// A fingerprint in groups of four, for reading aloud or comparing by eye.
pub fn grouped_fingerprint(fingerprint: &str) -> String {
    fingerprint
        .as_bytes()
        .chunks(4)
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const PINNED_FP: &str = "0123456789abcdef0123456789abcdef01234567";
    const SERVED_FP: &str = "89ABCDEF0123456789ABCDEF0123456789ABCDEF";

    #[test]
    fn stored_messages_read_back_as_the_error() {
        let error = PinnedKeyChanged::new(
            "bob@example.org",
            [PINNED_FP.to_owned()],
            [SERVED_FP.to_owned(), "AB".repeat(20)],
        );
        let message = error.to_string();
        assert!(message.contains("bob@example.org"), "{message}");
        assert!(
            message.contains(&PINNED_FP.to_ascii_uppercase()),
            "{message}"
        );
        assert_eq!(PinnedKeyChanged::of_message(&message), Some(error.clone()));
        assert_eq!(error.new_fingerprint(), Some(SERVED_FP));
    }

    #[test]
    fn other_messages_are_not_key_changes() {
        for message in [
            "",
            "failed to send the message through Proton",
            "The encryption key for bob@example.org changed: the contact card pins ; Proton serves AB. Trust the new key to send.",
            "The encryption key for  changed: the contact card pins AB; Proton serves CD. Trust the new key to send.",
            "The encryption key for bob changed: the contact card pins XY; Proton serves CD. Trust the new key to send.",
        ] {
            assert_eq!(PinnedKeyChanged::of_message(message), None, "{message}");
        }
    }

    #[test]
    fn trust_covers_one_address_and_one_key() {
        let trust = KeyTrust {
            recipient: "bob@example.org".to_owned(),
            fingerprint: SERVED_FP.to_owned(),
        };
        assert!(trust.covers("Bob@Example.org", &SERVED_FP.to_ascii_lowercase()));
        assert!(!trust.covers("bob@example.org", PINNED_FP));
        assert!(!trust.covers("alice@example.org", SERVED_FP));
    }

    #[test]
    fn groups_fingerprints_by_four() {
        assert_eq!(grouped_fingerprint("ABCDEF0123"), "ABCD EF01 23");
    }
}
