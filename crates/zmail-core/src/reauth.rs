//! Failures that only signing in again can fix.
//!
//! When a provider refuses an account's credentials, retrying the same
//! request never helps: the user has to Re-authenticate. Protocol crates
//! return a [`ReauthRequired`] error for these, sync stores its message
//! (outbox and sync errors are kept as plain text), and the shell
//! recognises the stored text with [`ReauthReason::of_message`] to offer
//! Re-authenticate instead of a generic error and Retry.

use std::fmt;

/// Why an account has to sign in again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReauthReason {
    /// The outgoing server refused the sign-in when sending (SMTP 535):
    /// a changed password, or an OAuth2 grant without permission to send,
    /// as Microsoft 365 accounts signed in before zmail asked for
    /// `SMTP.Send` have.
    SendRefused,
}

impl ReauthReason {
    /// Every reason, for recognising stored messages.
    const ALL: [ReauthReason; 1] = [ReauthReason::SendRefused];

    /// The sentence a stored message starts with. Changing it orphans
    /// messages already stored, which then read as ordinary errors.
    fn lead(self) -> &'static str {
        match self {
            ReauthReason::SendRefused => {
                "Sign in again to send: the mail server refused zmail's sign-in."
            }
        }
    }

    /// The reason a stored error message (an outbox job's or a sync
    /// status's) records, if it is one of these failures.
    pub fn of_message(message: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|reason| message.starts_with(reason.lead()))
    }
}

/// An error only signing in again can fix. Its message starts with the
/// reason's fixed sentence and ends with the server's own words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReauthRequired {
    /// Why.
    pub reason: ReauthReason,
    /// What the server said, for whoever wants it.
    pub detail: String,
}

impl ReauthRequired {
    /// An error for `reason` with the server's `detail`.
    pub fn new(reason: ReauthReason, detail: impl Into<String>) -> Self {
        Self {
            reason,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for ReauthRequired {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.reason.lead())?;
        let detail = self.detail.trim();
        if !detail.is_empty() {
            write!(f, " ({detail})")?;
        }
        Ok(())
    }
}

impl std::error::Error for ReauthRequired {}

#[cfg(test)]
mod tests {
    use super::{ReauthReason, ReauthRequired};

    #[test]
    fn stored_messages_are_recognised_by_their_lead() {
        let error = ReauthRequired::new(
            ReauthReason::SendRefused,
            "535 5.7.3 Authentication unsuccessful",
        );
        let message = error.to_string();
        assert!(message.ends_with("(535 5.7.3 Authentication unsuccessful)"));
        assert_eq!(
            ReauthReason::of_message(&message),
            Some(ReauthReason::SendRefused)
        );

        let bare = ReauthRequired::new(ReauthReason::SendRefused, " ").to_string();
        assert!(!bare.contains('('), "no empty detail: {bare}");
        assert_eq!(
            ReauthReason::of_message(&bare),
            Some(ReauthReason::SendRefused)
        );
    }

    #[test]
    fn other_errors_are_not_reauth_failures() {
        for message in [
            "",
            "failed to connect to the SMTP server using implicit TLS",
            "Connection refused",
        ] {
            assert_eq!(ReauthReason::of_message(message), None);
        }
    }
}
