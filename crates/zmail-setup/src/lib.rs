//! Adding mail accounts, shared by Zmail's frontends (ADR 0007, section 8):
//! everything about account setup that isn't drawing it. `zmail-ui` (the
//! GUI's onboarding and Settings → Accounts) and `zmail-tui` (Setup →
//! Accounts) both call it, so the two add accounts the same way. No GPUI.
//!
//! | Module | What it does |
//! | --- | --- |
//! | [`form`] | the form's fields, validation, and building `NewAccount` with its secret |
//! | [`providers`] | which preset an address belongs to, and each provider's app password help |
//! | [`discovery`] | driving `zmail_autodiscover` step by step, and how its progress reads |
//! | [`check`] | testing the sign-in before anything is stored |
//! | [`oauth`] | the OAuth2 browser sign-in (PKCE, loopback) step by step, and the SSH note |
//! | [`google`] | the guide to a Google OAuth client of your own, as data, and its checks |
//! | [`accounts`] | storing an account and its secret, the first sync, removing, signing in again |
//! | `proton` | the Proton sign-in's state machine (`proton` feature) |
//!
//! Rules both frontends rely on:
//!
//! - Secrets go to the keychain (`zmail_security::SecretStore`) and nowhere
//!   else; the form values' and secrets' `Debug` leave them out.
//! - Zmail ships no OAuth client ID or secret. Users bring their own; the
//!   form checks a pasted Google client's shape.
//! - Strings from servers (discovery results, errors, Proton's answers) are
//!   untrusted. A terminal filters them before drawing them
//!   (`zmail_security::terminal::sanitize`).

pub mod accounts;
pub mod check;
pub mod discovery;
pub mod form;
pub mod google;
pub mod oauth;
#[cfg(feature = "proton")]
pub mod proton;
pub mod providers;

pub use accounts::{SetupServices, SyncProgress};
pub use form::{
    AccountField, AccountFormValues, AccountSetupSecret, Check, build_account_from_form,
    validate_account_form,
};
