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
//!
//! Zmail also remembers what each verified card pinned and whether it
//! required encryption ([`RecipientPins`]), so a card that later goes
//! missing still stops a send to another key with [`PinnedKeyChanged`],
//! and a recipient whose card required encryption is never sent to in
//! clear ([`EncryptionKeyMissing`]).

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

/// What a recipient's verified Proton contact card asked for when a send
/// last used it: the keys it pins and whether it requires encryption.
///
/// Zmail keeps this per account (ADR 0004, "Contact cards that go
/// missing"), so that a later send whose card is gone, or no longer pins
/// keys, cannot quietly go to another key or in clear. Only the user
/// removes a record (or removing the account does); a later verified card
/// can replace the pins or turn "always encrypt" on, never take them away.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecipientPins {
    /// The recipient's address, trimmed and in lower case.
    pub recipient: String,
    /// The fingerprints the card pinned, upper-case hex. Empty when it
    /// only required encryption.
    pub fingerprints: Vec<String>,
    /// The card required encryption ("always encrypt").
    pub encrypt: bool,
}

impl RecipientPins {
    /// The record for `recipient`, with the address in lower case and the
    /// fingerprints in upper case, each once.
    pub fn new(
        recipient: &str,
        fingerprints: impl IntoIterator<Item = String>,
        encrypt: bool,
    ) -> Self {
        let mut unique: Vec<String> = Vec::new();
        for fingerprint in fingerprints {
            let fingerprint = fingerprint.to_ascii_uppercase();
            if !unique.contains(&fingerprint) {
                unique.push(fingerprint);
            }
        }
        Self {
            recipient: recipient.trim().to_lowercase(),
            fingerprints: unique,
            encrypt,
        }
    }

    /// Whether this is the record for `recipient`, whatever its case.
    pub fn is_for(&self, recipient: &str) -> bool {
        self.recipient.eq_ignore_ascii_case(recipient.trim())
    }

    /// The record to keep once a send used a verified card for
    /// `recipient` that pins `card_fingerprints` and asks for encryption
    /// when `card_encrypt`, given the record kept so far; `None` when
    /// nothing changes. A card that pins keys replaces the pins kept; a
    /// card that pins none keeps them. "Always encrypt" is turned on by a
    /// card and stays on.
    pub fn seen_on_card(
        recipient: &str,
        card_fingerprints: impl IntoIterator<Item = String>,
        card_encrypt: bool,
        remembered: Option<&Self>,
    ) -> Option<Self> {
        let card_fingerprints: Vec<String> = card_fingerprints.into_iter().collect();
        if card_fingerprints.is_empty() && !card_encrypt {
            return None;
        }
        let fingerprints = if card_fingerprints.is_empty() {
            remembered
                .map(|remembered| remembered.fingerprints.clone())
                .unwrap_or_default()
        } else {
            card_fingerprints
        };
        let encrypt = card_encrypt || remembered.is_some_and(|remembered| remembered.encrypt);
        let updated = Self::new(recipient, fingerprints, encrypt);
        (remembered != Some(&updated)).then_some(updated)
    }
}

/// The account's saved key records a Proton send reads: the served keys
/// the user trusted, and the pins Zmail remembers from verified contact
/// cards. The send notes here what it saw on verified cards for the
/// caller to save, whether or not the send then goes.
#[derive(Debug, Default)]
pub struct RecipientKeys {
    /// The account's key trust decisions.
    pub trusted: Vec<KeyTrust>,
    /// The pins remembered for the account's recipients.
    pub pins: Vec<RecipientPins>,
    noted: std::sync::Mutex<Vec<RecipientPins>>,
}

impl RecipientKeys {
    /// The records a send starts from.
    pub fn new(trusted: Vec<KeyTrust>, pins: Vec<RecipientPins>) -> Self {
        Self {
            trusted,
            pins,
            noted: std::sync::Mutex::default(),
        }
    }

    /// The pins remembered for `recipient`, whatever its case.
    pub fn pins_for(&self, recipient: &str) -> Option<&RecipientPins> {
        self.pins.iter().find(|pins| pins.is_for(recipient))
    }

    /// Notes a record to save; a later one for the same address replaces
    /// it.
    pub fn note(&self, pins: RecipientPins) {
        let mut noted = self
            .noted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match noted
            .iter_mut()
            .find(|noted| noted.recipient == pins.recipient)
        {
            Some(earlier) => *earlier = pins,
            None => noted.push(pins),
        }
    }

    /// The records noted so far, which are then cleared.
    pub fn take_noted(&self) -> Vec<RecipientPins> {
        std::mem::take(
            &mut *self
                .noted
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }
}

/// The fixed parts of a stored [`EncryptionKeyMissing`] message. Changing
/// them orphans messages already stored, which then read as ordinary
/// errors.
const MISSING_LEAD: &str = "Not sent: your Proton contact for ";
const MISSING_MIDDLE: &str = " asked for encrypted mail when Zmail last sent to them, and Zmail now has no key to encrypt this message to. Check the contact in the Proton Mail web app, or forget the keys Zmail saved for ";
const MISSING_TAIL: &str = ", then send again.";

/// A send stopped because Zmail remembers that a recipient's contact card
/// required encryption (or pinned keys it must encrypt to), and nothing
/// now gives it a key it may encrypt to: the card is gone and Proton
/// serves no key. Zmail never sends such a message in clear.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptionKeyMissing {
    /// The recipient's address.
    pub recipient: String,
}

impl EncryptionKeyMissing {
    /// The error for `recipient`.
    pub fn new(recipient: impl Into<String>) -> Self {
        Self {
            recipient: recipient.into(),
        }
    }

    /// The error a stored outbox message records, if it is one.
    pub fn of_message(message: &str) -> Option<Self> {
        let rest = message
            .strip_prefix(MISSING_LEAD)?
            .strip_suffix(MISSING_TAIL)?;
        let (recipient, again) = rest.split_once(MISSING_MIDDLE)?;
        (!recipient.is_empty() && recipient == again).then(|| Self::new(recipient))
    }
}

impl fmt::Display for EncryptionKeyMissing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{MISSING_LEAD}{0}{MISSING_MIDDLE}{0}{MISSING_TAIL}",
            self.recipient
        )
    }
}

impl std::error::Error for EncryptionKeyMissing {}

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
    fn a_verified_card_updates_the_remembered_pins_but_never_drops_them() {
        let pinned = || vec![PINNED_FP.to_owned()];
        // A card that pins a key and asks for encryption is remembered,
        // under the address in lower case.
        let first =
            RecipientPins::seen_on_card(" Bob@Example.org ", pinned(), true, None).expect("saved");
        assert_eq!(
            first,
            RecipientPins {
                recipient: "bob@example.org".to_owned(),
                fingerprints: vec![PINNED_FP.to_ascii_uppercase()],
                encrypt: true,
            }
        );
        // The same card again: nothing new to save.
        assert_eq!(
            RecipientPins::seen_on_card("bob@example.org", pinned(), true, Some(&first)),
            None
        );
        // A card that pins nothing and does not ask for encryption is not
        // remembered, and leaves an earlier record as it is.
        assert_eq!(
            RecipientPins::seen_on_card("bob@example.org", Vec::new(), false, None),
            None
        );
        assert_eq!(
            RecipientPins::seen_on_card("bob@example.org", Vec::new(), false, Some(&first)),
            None
        );
        // New pins replace the old ones; "always encrypt" stays.
        let moved = RecipientPins::seen_on_card(
            "bob@example.org",
            vec![SERVED_FP.to_owned()],
            false,
            Some(&first),
        )
        .expect("updated");
        assert_eq!(moved.fingerprints, [SERVED_FP]);
        assert!(moved.encrypt);
        // "Always encrypt" with no pins keeps the pins remembered.
        let unencrypted = RecipientPins {
            encrypt: false,
            ..first.clone()
        };
        let kept =
            RecipientPins::seen_on_card("bob@example.org", Vec::new(), true, Some(&unencrypted))
                .expect("updated");
        assert_eq!(kept.fingerprints, first.fingerprints);
        assert!(kept.encrypt);
    }

    #[test]
    fn recipient_keys_find_a_recipient_whatever_the_case_and_collect_what_a_send_saw() {
        let pins = RecipientPins::new("bob@example.org", [PINNED_FP.to_owned()], true);
        let keys = RecipientKeys::new(Vec::new(), vec![pins.clone()]);
        assert_eq!(keys.pins_for("BOB@example.org"), Some(&pins));
        assert_eq!(keys.pins_for("alice@example.org"), None);
        assert!(keys.take_noted().is_empty());
        let moved = RecipientPins::new("bob@example.org", [SERVED_FP.to_owned()], true);
        keys.note(pins.clone());
        // A second note for the same address replaces the first.
        keys.note(moved.clone());
        keys.note(RecipientPins::new("alice@example.org", [], true));
        let noted = keys.take_noted();
        assert_eq!(noted.len(), 2);
        assert_eq!(noted[0], moved);
        assert!(keys.take_noted().is_empty(), "taken once");
    }

    #[test]
    fn stored_missing_key_messages_read_back_as_the_error() {
        let error = EncryptionKeyMissing::new("bob@example.org");
        let message = error.to_string();
        assert!(message.starts_with("Not sent: "), "{message}");
        assert!(message.contains("bob@example.org"), "{message}");
        assert!(message.contains("Proton Mail web app"), "{message}");
        assert!(!message.contains('\u{2014}'), "no dashes: {message}");
        assert_eq!(EncryptionKeyMissing::of_message(&message), Some(error));
        for message in [
            "",
            "failed to send the message through Proton",
            &PinnedKeyChanged::new(
                "bob@example.org",
                [PINNED_FP.to_owned()],
                [SERVED_FP.to_owned()],
            )
            .to_string(),
            &EncryptionKeyMissing::new("").to_string(),
        ] {
            assert_eq!(EncryptionKeyMissing::of_message(message), None, "{message}");
        }
    }

    #[test]
    fn groups_fingerprints_by_four() {
        assert_eq!(grouped_fingerprint("ABCDEF0123"), "ABCD EF01 23");
    }
}
