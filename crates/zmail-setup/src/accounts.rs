//! Storing, watching and removing accounts: what happens once the form
//! is filled in, the same for both frontends.
//!
//! - [`store_account`] stores a validated form, puts its secret in the
//!   keychain (never anywhere else) and starts the first sync, or the
//!   OAuth2 browser flow that ends in it.
//! - [`read_sync_progress`] reads the first sync's progress from the store.
//! - [`backfill`] reads how much of a Proton account's older mail is
//!   still to fetch, for the account's details.
//! - [`remove_account`] removes an account's local data and every keychain
//!   secret it has.
//!
//! The first sync starts through `SyncService::sync_account`. Built with
//! `SyncService::with_sync_lock`, a frontend that doesn't hold the store's
//! sync lock leaves it to the process that does, which picks up accounts
//! another process adds.

use std::sync::Arc;

use anyhow::Result;
use zmail_core::{
    AccountSummary, AuthKind, SyncPhase,
    text::{count_noun, thousands},
};
use zmail_security::{SecretSlot, SecretStore};
use zmail_store::{AccountRemovalImpact, LocalStore};
use zmail_sync::SyncService;

use crate::form::{AccountFormValues, AccountSetupSecret, build_account_from_form};

/// What a frontend needs to add accounts.
#[derive(Clone)]
pub struct SetupServices {
    /// The open store.
    pub store: Arc<LocalStore>,
    /// The keychain: where account secrets go, and only there.
    pub secrets: Arc<dyn SecretStore>,
    /// Sync, for the first sync of a new account.
    pub sync_service: SyncService,
    /// What autodiscovery talks to; tests swap in fakes.
    pub network: zmail_autodiscover::Network,
}

/// Stores a validated form and returns the new account id and the status
/// message for the user. Then starts the first sync, or the OAuth2 browser
/// flow that ends in it, when there is a sync service (a frontend reading
/// a fixture store has none).
pub fn store_account(
    store: &Arc<LocalStore>,
    secrets: &Arc<dyn SecretStore>,
    sync_service: Option<&SyncService>,
    form: &AccountFormValues,
) -> Result<(i64, String)> {
    let (account, secret) = build_account_from_form(form)?;
    let account_id = store.insert_account(&account)?;
    let message = match secret {
        AccountSetupSecret::Password(secret) => {
            secrets.set_account_secret(account_id, &secret)?;
            if let Some(sync_service) = sync_service {
                let _ = sync_service.sync_account(account_id);
            }
            format!("{} added.", account.display_name)
        }
        AccountSetupSecret::OAuth2(client) => {
            // The browser flow blocks for up to its redirect timeout, so it
            // runs off the UI thread; the account's sync status marks the
            // wait, and the grant (tokens, client config, expiry) is
            // persisted before the first sync starts. Syncing here would
            // only record a missing-credentials failure.
            let _ = store.begin_sync_attempt(
                account_id,
                SyncPhase::Connecting,
                "Waiting for browser authorization",
            );
            spawn_oauth2_authorization(store, secrets, sync_service.cloned(), account_id, client);
            format!(
                "{} added. Finish signing in in your browser; the account syncs once you approve access.",
                account.display_name
            )
        }
    };
    // Start the new account's background watcher immediately instead of
    // waiting for the next app launch.
    if let Some(sync_service) = sync_service {
        let _ = sync_service.sync_watchers();
    }
    Ok((account_id, message))
}

/// Stores a validated OAuth2 form whose browser sign-in already finished
/// (the terminal UI signs in before saving, so it can test the
/// connection first): the account, then its grant, then the first sync.
/// An account whose grant can't be kept is removed again.
pub fn store_authorized_account(
    store: &LocalStore,
    secrets: &dyn SecretStore,
    sync_service: Option<&SyncService>,
    form: &AccountFormValues,
    tokens: &zmail_auth::TokenSet,
) -> Result<(i64, String)> {
    let (account, secret) = build_account_from_form(form)?;
    let AccountSetupSecret::OAuth2(client) = secret else {
        anyhow::bail!("this account signs in with a password");
    };
    let account_id = store.insert_account(&account)?;
    if let Err(error) =
        zmail_sync::persist_oauth2_grant(store, secrets, account_id, &client, tokens)
    {
        let _ = remove_account(store, secrets, account_id);
        return Err(error.context("Zmail could not keep the sign-in in the keychain"));
    }
    if let Some(sync_service) = sync_service {
        let _ = sync_service.sync_account(account_id);
        let _ = sync_service.sync_watchers();
    }
    Ok((account_id, format!("{} added.", account.display_name)))
}

/// Runs the OAuth2 browser flow on a background thread and stores the
/// resulting grant, so the UI never blocks on the redirect wait.
fn spawn_oauth2_authorization(
    store: &Arc<LocalStore>,
    secrets: &Arc<dyn SecretStore>,
    sync_service: Option<SyncService>,
    account_id: i64,
    client: zmail_auth::OAuth2Client,
) {
    let store_for_thread = store.clone();
    let secrets = secrets.clone();
    let spawn = std::thread::Builder::new()
        .name("zmail-oauth2-setup".to_owned())
        .spawn(move || {
            let store = store_for_thread;
            let tokens = match client.authorize_interactively() {
                Ok(tokens) => tokens,
                Err(error) => {
                    let _ = store.record_sync_failure(
                        account_id,
                        "Browser authorization failed",
                        error.to_string(),
                    );
                    return;
                }
            };
            if let Err(error) = zmail_sync::persist_oauth2_grant(
                &store,
                secrets.as_ref(),
                account_id,
                &client,
                &tokens,
            ) {
                let _ = store.record_sync_failure(
                    account_id,
                    "Storing the authorization failed",
                    error.to_string(),
                );
                return;
            }
            if let Some(sync_service) = sync_service {
                let _ = sync_service.sync_account(account_id);
                let _ = sync_service.sync_watchers();
            }
        });
    if let Err(error) = spawn {
        let _ = store.record_sync_failure(
            account_id,
            "Browser authorization failed",
            error.to_string(),
        );
    }
}

// ---------------------------------------------------------------------------
// The first sync
// ---------------------------------------------------------------------------

/// The first sync's progress, read from the store.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncProgress {
    /// The sync's phase.
    pub phase: Option<SyncPhase>,
    /// What it is doing (from the sync, so untrusted text).
    pub detail: String,
    /// Why it failed (from the server, so untrusted text).
    pub error: Option<String>,
    /// Folders found so far.
    pub folders: usize,
    /// Conversations found so far.
    pub conversations: usize,
    /// The sync has finished at least once.
    pub done: bool,
}

/// Reads an account's sync progress, or `None` when it is gone.
pub fn read_sync_progress(store: &LocalStore, account_id: i64) -> Option<SyncProgress> {
    let account = store
        .list_accounts()
        .ok()?
        .into_iter()
        .find(|account| account.id == account_id)?;
    let mailboxes = store.list_mailboxes(account_id).unwrap_or_default();
    let status = account.sync_status;
    Some(SyncProgress {
        phase: Some(status.phase),
        detail: status.detail,
        error: (status.phase == SyncPhase::Failed)
            .then(|| status.last_error_message.unwrap_or_default()),
        folders: mailboxes.len(),
        conversations: mailboxes.iter().map(|mailbox| mailbox.total_threads).sum(),
        done: status.phase == SyncPhase::Idle && status.last_success_at.is_some(),
    })
}

/// How far an account's older mail has got. A Proton account fetches its
/// newest mail first and older mail in the background, keeping the count
/// in its sync checkpoint (docs/adr/0004-proton-native.md); IMAP and JMAP
/// accounts list everything in each sync, so they have none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backfill {
    /// Messages listed so far.
    pub fetched: usize,
    /// The account's message count when its first sync started, when
    /// known.
    pub total: Option<usize>,
    /// The whole history is in.
    pub complete: bool,
}

impl Backfill {
    /// Messages still to fetch, when the total is known.
    pub fn remaining(&self) -> Option<usize> {
        if self.complete {
            return Some(0);
        }
        self.total.map(|total| total.saturating_sub(self.fetched))
    }

    /// "All older mail fetched", "1,234 of 85,000 messages still to
    /// fetch", or, when the total is unknown or overtaken by new mail,
    /// "2,048 fetched so far, more to come".
    pub fn label(&self) -> String {
        match (self.complete, self.total, self.remaining()) {
            (true, _, _) => "All older mail fetched".to_owned(),
            (false, Some(total), Some(remaining)) if remaining > 0 => format!(
                "{} of {} still to fetch",
                thousands(remaining),
                count_noun_thousands(total, "message", "messages")
            ),
            _ => format!("{} fetched so far, more to come", thousands(self.fetched)),
        }
    }
}

/// `count_noun` with a grouped count: `85,000 messages`.
fn count_noun_thousands(count: usize, singular: &str, plural: &str) -> String {
    format!(
        "{} {}",
        thousands(count),
        if count == 1 { singular } else { plural }
    )
}

/// Reads how far `account`'s older mail has got from its stored sync
/// checkpoint, or `None` for an account without a backfill (IMAP, JMAP,
/// a build without Proton) or one that hasn't synced yet.
pub fn backfill(account: &AccountSummary) -> Option<Backfill> {
    match account.protocol {
        #[cfg(feature = "proton")]
        zmail_core::ProtocolKind::Proton => {
            let checkpoint =
                zmail_proton::ProtonCheckpoint::parse(account.sync_status.checkpoint.as_deref()?)?;
            Some(Backfill {
                fetched: checkpoint.listed,
                total: checkpoint.total,
                complete: !checkpoint.backfill_pending(),
            })
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Removing and signing in again
// ---------------------------------------------------------------------------

/// What removing `name` deletes, for the confirmation.
pub fn removal_description(name: &str) -> String {
    format!(
        "This deletes {name}'s local data from this computer: its downloaded mail, attachments and search index, and its password and sign-in tokens in the system keychain. Mail on the server is not touched, and adding the account again downloads it again."
    )
}

/// The unsent mail and drafts that go with the account, which exist only
/// on this computer.
pub fn removal_lines(impact: &AccountRemovalImpact) -> Vec<String> {
    let mut lines = Vec::new();
    if impact.unsent_messages > 0 {
        lines.push(format!(
            "{} in the Outbox will be deleted without being sent.",
            count_noun(impact.unsent_messages, "unsent message", "unsent messages")
        ));
    }
    if impact.drafts > 0 {
        lines.push(format!(
            "{} will be deleted.",
            count_noun(impact.drafts, "draft", "drafts")
        ));
    }
    lines
}

/// Why an account can't be removed right now, if it can't.
pub fn removal_blocker(impact: &AccountRemovalImpact) -> Option<String> {
    if impact.sending_messages > 0 {
        return Some(
            "A message from this account is being sent. Try again once it has gone.".to_owned(),
        );
    }
    None
}

/// Why removing an account failed.
#[derive(Debug)]
pub enum RemoveError {
    /// The local data couldn't be deleted; nothing was removed.
    LocalData(anyhow::Error),
    /// The local data is gone, but some keychain entries remain.
    Keychain(anyhow::Error),
}

/// Removes an account from this computer: its local data, then every
/// keychain secret it has, a Proton account's session too. The mail on
/// the server is untouched.
pub fn remove_account(
    store: &LocalStore,
    secrets: &dyn SecretStore,
    account_id: i64,
) -> Result<(), RemoveError> {
    store
        .delete_account(account_id)
        .map_err(RemoveError::LocalData)?;
    let mut forgot = secrets.delete_account_secret(account_id);
    for slot in [
        SecretSlot::Password,
        SecretSlot::ClientSecret,
        SecretSlot::AccessToken,
        SecretSlot::RefreshToken,
        SecretSlot::ProtonUid,
        SecretSlot::ProtonRefreshToken,
        SecretSlot::ProtonKeyPass,
    ] {
        forgot = forgot.and(secrets.delete_account_secret_in_slot(account_id, slot));
    }
    forgot.map_err(RemoveError::Keychain)
}

/// The OAuth2 client to sign an account in again with: the stored client
/// configuration with its secret from the keychain, and its provider.
/// Only the provider presets (Gmail, Microsoft 365) can start again; a
/// custom OAuth service is removed and added again.
pub fn reauthorization_client(
    store: &LocalStore,
    secrets: &dyn SecretStore,
    account_id: i64,
) -> Result<(zmail_auth::OAuth2Preset, zmail_auth::OAuth2Client), &'static str> {
    let config = store
        .sync_target(account_id)
        .ok()
        .flatten()
        .and_then(|target| target.oauth2);
    let Some((preset, config)) = config.and_then(|config| {
        zmail_auth::OAuth2Preset::for_token_endpoint(&config.token_endpoint)
            .map(|preset| (preset, config))
    }) else {
        return Err(
            "This account signs in through a custom OAuth service, so Zmail cannot start its sign-in again. Remove the account and add it again.",
        );
    };
    let client_secret = secrets
        .get_account_secret_from_slot(account_id, SecretSlot::ClientSecret)
        .ok();
    let client = preset.client(
        config.client_id,
        client_secret,
        zmail_auth::DEFAULT_REDIRECT_PORT,
    );
    Ok((preset, client))
}

/// Keeps a new OAuth2 grant for an account signed in again, then syncs it,
/// with the messages refused for the old sign-in sent again. On failure
/// the account's sync status says so.
pub fn finish_reauthorization(
    store: &LocalStore,
    secrets: &dyn SecretStore,
    sync_service: &SyncService,
    account_id: i64,
    client: &zmail_auth::OAuth2Client,
    tokens: Result<zmail_auth::TokenSet>,
) {
    let result = tokens.and_then(|tokens| {
        zmail_sync::persist_oauth2_grant(store, secrets, account_id, client, &tokens)
    });
    match result {
        Ok(()) => {
            let _ = sync_service.sync_account(account_id);
            let _ = sync_service.sync_watchers();
            // Messages refused for the old sign-in go now.
            let _ = sync_service.retry_refused_sends(account_id);
        }
        Err(error) => {
            let _ = store.record_sync_failure(
                account_id,
                "Browser authorization failed",
                error.to_string(),
            );
        }
    }
}

/// Saves a new password for a password account and syncs it again, with
/// any messages its server refused retried.
pub fn save_password(
    secrets: &dyn SecretStore,
    sync_service: &SyncService,
    account_id: i64,
    password: &str,
) -> Result<()> {
    secrets.set_account_secret(account_id, password)?;
    let _ = sync_service.sync_account(account_id);
    let _ = sync_service.sync_watchers();
    let _ = sync_service.retry_refused_sends(account_id);
    Ok(())
}

/// How an account signs in, as Settings words it.
pub fn auth_label(auth: AuthKind) -> &'static str {
    match auth {
        AuthKind::Password => "Password",
        AuthKind::OAuth2 => "OAuth2",
        AuthKind::ProtonSession => "Proton sign-in",
    }
}

/// The cache budgets offered, in MiB; `None` is no limit.
pub const CACHE_BUDGETS: [Option<u64>; 7] = [
    None,
    Some(250),
    Some(500),
    Some(1024),
    Some(2 * 1024),
    Some(5 * 1024),
    Some(10 * 1024),
];

/// The budgets a picker lists: [`CACHE_BUDGETS`], plus the saved one when
/// it is not among them.
pub fn budget_options(saved_mb: Option<u64>) -> Vec<Option<u64>> {
    let mut options = CACHE_BUDGETS.to_vec();
    if !options.contains(&saved_mb) {
        options.push(saved_mb);
        options.sort_by_key(|budget| budget.unwrap_or(0));
    }
    options
}

/// How a cache budget reads in the picker.
pub fn budget_label(budget_mb: Option<u64>) -> String {
    match budget_mb {
        None => "No limit".to_owned(),
        Some(mb) if mb >= 1024 && mb % 1024 == 0 => format!("{} GB", mb / 1024),
        Some(mb) => format!("{mb} MB"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use zmail_core::{AuthKind, CachePolicySummary, NewAccount, NewIdentity, ProtocolKind};
    use zmail_security::{InMemorySecretStore, SecretSlot, SecretStore};
    use zmail_store::{AccountRemovalImpact, LocalStore};

    use super::{
        Backfill, CACHE_BUDGETS, backfill, budget_label, budget_options, read_sync_progress,
        removal_blocker, removal_description, removal_lines, remove_account,
    };

    fn account() -> NewAccount {
        NewAccount {
            display_name: "jane@example.test".to_owned(),
            protocol: ProtocolKind::Imap,
            auth: AuthKind::Password,
            endpoint: "imaps://imap.example.test:993".to_owned(),
            username: "jane@example.test".to_owned(),
            cache_policy: CachePolicySummary::performance_first(),
            outgoing: None,
            identities: vec![NewIdentity {
                display_name: "Jane".to_owned(),
                email_address: "jane@example.test".to_owned(),
                signature: String::new(),
                is_primary: true,
            }],
        }
    }

    #[test]
    fn budgets_read_in_megabytes_and_gigabytes() {
        assert_eq!(budget_label(None), "No limit");
        assert_eq!(budget_label(Some(250)), "250 MB");
        assert_eq!(budget_label(Some(2048)), "2 GB");
        assert_eq!(budget_label(Some(1500)), "1500 MB");
        assert_eq!(budget_options(Some(300)).len(), CACHE_BUDGETS.len() + 1);
        assert_eq!(budget_options(Some(500)).len(), CACHE_BUDGETS.len());
    }

    #[test]
    fn the_confirmation_lists_unsent_mail_and_drafts() {
        let impact = AccountRemovalImpact {
            unsent_messages: 2,
            sending_messages: 0,
            drafts: 1,
        };
        assert_eq!(
            removal_lines(&impact),
            [
                "2 unsent messages in the Outbox will be deleted without being sent.",
                "1 draft will be deleted.",
            ]
        );
        assert_eq!(removal_blocker(&impact), None);
        assert!(
            removal_blocker(&AccountRemovalImpact {
                sending_messages: 1,
                ..impact
            })
            .is_some()
        );
        assert!(removal_lines(&AccountRemovalImpact::default()).is_empty());
        let description = removal_description("Work");
        assert!(description.contains("Mail on the server is not touched"));
        assert!(description.contains("keychain"));
    }

    #[test]
    fn removing_an_account_forgets_its_secrets() {
        let store = LocalStore::open_ephemeral().expect("a store");
        let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
        let account_id = store.insert_account(&account()).expect("stored");
        secrets
            .set_account_secret(account_id, "app-password")
            .expect("kept");
        for slot in [SecretSlot::RefreshToken, SecretSlot::ProtonKeyPass] {
            secrets
                .set_account_secret_in_slot(account_id, slot, "token")
                .expect("kept");
        }
        assert!(read_sync_progress(&store, account_id).is_some());

        assert!(remove_account(&store, secrets.as_ref(), account_id).is_ok());
        assert!(store.list_accounts().expect("accounts").is_empty());
        assert!(secrets.get_account_secret(account_id).is_err());
        for slot in [SecretSlot::RefreshToken, SecretSlot::ProtonKeyPass] {
            assert!(
                secrets
                    .get_account_secret_from_slot(account_id, slot)
                    .is_err(),
                "{slot:?} is forgotten"
            );
        }
        assert!(read_sync_progress(&store, account_id).is_none());
    }
    #[test]
    fn a_stored_account_keeps_its_password_in_the_keychain_only() {
        let store = Arc::new(LocalStore::open_ephemeral().expect("a store"));
        let secrets: Arc<dyn SecretStore> = InMemorySecretStore::shared();
        let form = crate::form::AccountFormValues {
            name: "Jane".to_owned(),
            email: "jane@example.test".to_owned(),
            password: "app-password".to_owned(),
            imap_host: "imap.example.test".to_owned(),
            smtp_host: "smtp.example.test".to_owned(),
            ..crate::form::AccountFormValues::default()
        };
        let (account_id, message) =
            super::store_account(&store, &secrets, None, &form).expect("stored");
        assert_eq!(message, "jane@example.test added.");
        assert_eq!(
            secrets.get_account_secret(account_id).expect("kept"),
            "app-password"
        );
        let account = &store.list_accounts().expect("accounts")[0];
        assert_eq!(account.username, "jane@example.test");
    }

    #[test]
    fn an_authorized_account_keeps_its_grant() {
        let store = LocalStore::open_ephemeral().expect("a store");
        let secrets = InMemorySecretStore::shared();
        let form = crate::form::AccountFormValues {
            email: "jane@gmail.com".to_owned(),
            provider: Some(zmail_autodiscover::Provider::Google),
            use_oauth: true,
            oauth_client_id: "42-abc.apps.googleusercontent.com".to_owned(),
            oauth_client_secret: "GOCSPX-x".to_owned(),
            imap_host: "imap.gmail.com".to_owned(),
            smtp_host: "smtp.gmail.com".to_owned(),
            ..crate::form::AccountFormValues::default()
        };
        let tokens = zmail_auth::TokenSet {
            access_token: "access".to_owned(),
            refresh_token: Some("refresh".to_owned()),
            expires_in: Some(3600),
        };
        let (account_id, _) =
            super::store_authorized_account(&store, secrets.as_ref(), None, &form, &tokens)
                .expect("stored");
        assert_eq!(
            secrets
                .get_account_secret_from_slot(account_id, SecretSlot::RefreshToken)
                .expect("kept"),
            "refresh"
        );
        // A password form can't be stored this way.
        let password = crate::form::AccountFormValues {
            use_oauth: false,
            password: "x".to_owned(),
            ..form
        };
        assert!(
            super::store_authorized_account(&store, secrets.as_ref(), None, &password, &tokens)
                .is_err()
        );
    }

    /// An account of `protocol` whose last sync stored `checkpoint`.
    fn summary(protocol: ProtocolKind, checkpoint: Option<&str>) -> zmail_core::AccountSummary {
        zmail_core::AccountSummary {
            id: 1,
            display_name: "Jane".to_owned(),
            protocol,
            endpoint: String::new(),
            username: "jane@example.test".to_owned(),
            identity_count: 1,
            cache_policy: CachePolicySummary::performance_first(),
            sync_status: zmail_core::SyncStatusSummary {
                checkpoint: checkpoint.map(str::to_owned),
                ..zmail_core::SyncStatusSummary::idle("")
            },
            color_override: None,
        }
    }

    #[test]
    fn backfill_labels_say_how_much_older_mail_is_left() {
        let pending = Backfill {
            fetched: 1_536,
            total: Some(85_000),
            complete: false,
        };
        assert_eq!(pending.remaining(), Some(83_464));
        assert_eq!(pending.label(), "83,464 of 85,000 messages still to fetch");
        let unknown = Backfill {
            total: None,
            ..pending
        };
        assert_eq!(unknown.remaining(), None);
        assert_eq!(unknown.label(), "1,536 fetched so far, more to come");
        let overtaken = Backfill {
            total: Some(1_000),
            ..pending
        };
        assert_eq!(overtaken.label(), "1,536 fetched so far, more to come");
        let complete = Backfill {
            complete: true,
            ..pending
        };
        assert_eq!(complete.remaining(), Some(0));
        assert_eq!(complete.label(), "All older mail fetched");
    }

    #[test]
    fn only_a_synced_proton_account_has_a_backfill() {
        assert_eq!(backfill(&summary(ProtocolKind::Imap, Some("42"))), None);
        assert_eq!(backfill(&summary(ProtocolKind::Proton, None)), None);
    }

    #[cfg(feature = "proton")]
    #[test]
    fn a_proton_backfill_is_read_from_the_checkpoint() {
        let pending = summary(
            ProtocolKind::Proton,
            Some(r#"{"event_id":"ev","backfill_end_id":"m9","listed":512,"total":2000}"#),
        );
        assert_eq!(
            backfill(&pending),
            Some(Backfill {
                fetched: 512,
                total: Some(2_000),
                complete: false,
            })
        );
        let complete = summary(
            ProtocolKind::Proton,
            Some(r#"{"event_id":"ev","listed":2000,"total":2000}"#),
        );
        assert_eq!(backfill(&complete).map(|fill| fill.complete), Some(true));
        // Zmail 0.17.0 and earlier stored the bare event ID: history complete.
        let legacy = summary(ProtocolKind::Proton, Some("ACXDmTaBub14w=="));
        assert_eq!(backfill(&legacy).map(|fill| fill.complete), Some(true));
    }
}
