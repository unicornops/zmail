// Ported from go-proton-api `unlock.go` at
// 8a62023d3e38cb29749bcfc01bf44eafda6cec82
// (https://github.com/ProtonMail/go-proton-api), Copyright (c) 2020 James
// Houlahan, Copyright (c) 2022 Proton AG, MIT License
// (crates/zmail-proton-api/LICENSE-MIT); and from proton-bridge
// `internal/bridge/user.go` (`LoginFull` key password choice and
// `loginUser` salting) at v3.27.1
// (43b9237786204a0215abfd4dcc51ee7bb92f52d4), Copyright (c) 2026 Proton AG,
// GPL-3.0-or-later, used under GPL-3.0-only.
// Modified by UnicornOps on 2026-10-20: rewritten in Rust on rPGP. Address
// keys are unlocked one after another rather than in parallel.

//! Unlocking an account: the user keys, then each address's keys.
//!
//! The order follows Bridge:
//!
//! 1. Pick the key password: the login password in one-password mode, or
//!    the separate mailbox password in two-password mode ([`key_password`]).
//! 2. Salt it with the primary user key's salt ([`Client::salted_key_pass`]);
//!    this is the secret Zmail stores, never the password.
//! 3. Unlock the user keys with it, then every address's keys with the user
//!    keys (token) or the salted key password (legacy) ([`unlock`]).

use std::collections::HashMap;

use crate::{
    address::Address, client::Client, keyring::KeyRing, manager_auth::PasswordMode,
    response::Error, salt::SaltedKeyPass, user::User,
};

/// Picks the password that unlocks the keys (Bridge's `LoginFull`): the
/// login password in one-password mode, the mailbox password in
/// two-password mode.
pub fn key_password<'a>(
    mode: PasswordMode,
    login_password: &'a str,
    mailbox_password: Option<&'a str>,
) -> Result<&'a str, Error> {
    match mode {
        PasswordMode::One => Ok(login_password),
        PasswordMode::Two => mailbox_password.ok_or(Error::MailboxPasswordRequired),
    }
}

impl Client {
    /// Salts the key password with the primary user key's salt (Bridge's
    /// `loginUser`) and checks that it unlocks the user keys.
    pub async fn salted_key_pass(
        &self,
        user: &User,
        key_pass: &str,
    ) -> Result<SaltedKeyPass, Error> {
        let primary = user
            .keys
            .primary()
            .ok_or_else(|| Error::Key("the user has no primary key".to_owned()))?;
        let salts = self.get_salts().await?;
        let salted = salts.salt_for_key(key_pass, &primary.id)?;
        user.keys
            .unlock(salted.as_bytes(), None)
            .map_err(|_| Error::KeyPasswordWrong)?;
        Ok(salted)
    }
}

/// The unlocked user keys and, per address ID, that address's keys.
#[derive(Debug)]
pub struct UnlockedAccount {
    /// The user keys.
    pub user: KeyRing,
    /// The address keys, by address ID. Addresses whose keys could not be
    /// unlocked are missing.
    pub addresses: HashMap<String, KeyRing>,
}

/// Unlocks the user keys, then the address keys (`Unlock`). Fails if no
/// user key or no address key unlocks.
pub fn unlock(
    user: &User,
    addresses: &[Address],
    salted_key_pass: &SaltedKeyPass,
) -> Result<UnlockedAccount, Error> {
    let user_keyring = user
        .keys
        .unlock(salted_key_pass.as_bytes(), None)
        .map_err(|error| Error::Key(format!("failed to unlock user keys: {error}")))?;
    let addresses: HashMap<String, KeyRing> = addresses
        .iter()
        .filter_map(|address| {
            address
                .keys
                .try_unlock(salted_key_pass.as_bytes(), Some(&user_keyring))
                .filter(|keyring| !keyring.is_empty())
                .map(|keyring| (address.id.clone(), keyring))
        })
        .collect();
    if addresses.is_empty() {
        return Err(Error::Key("failed to unlock any address keys".to_owned()));
    }
    Ok(UnlockedAccount {
        user: user_keyring,
        addresses,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        address::Address,
        keyring::{Keys, tests::*},
        user::User,
    };

    fn user(keys: Keys) -> User {
        User {
            id: "user".to_owned(),
            keys,
            ..User::default()
        }
    }

    #[test]
    fn picks_the_key_password_by_mode() {
        assert_eq!(
            key_password(PasswordMode::One, "login", None).ok(),
            Some("login")
        );
        assert_eq!(
            key_password(PasswordMode::One, "login", Some("mailbox")).ok(),
            Some("login")
        );
        assert_eq!(
            key_password(PasswordMode::Two, "login", Some("mailbox")).ok(),
            Some("mailbox")
        );
        assert!(matches!(
            key_password(PasswordMode::Two, "login", None),
            Err(Error::MailboxPasswordRequired)
        ));
    }

    #[test]
    fn unlocks_user_then_address_keys() {
        // The user key opens with "password" here, so the fixture's token
        // address key can be unlocked through it; a second address with a
        // legacy key and a third with nothing usable are also present.
        let account = unlock(
            &user(Keys(vec![key("user", USER_KEY_PASSWORD)])),
            &[
                Address {
                    id: "addr-token".to_owned(),
                    keys: Keys(vec![address_key()]),
                    ..Address::default()
                },
                Address {
                    id: "addr-legacy".to_owned(),
                    keys: Keys(vec![key("legacy", USER_KEY_PASSWORD)]),
                    ..Address::default()
                },
                Address {
                    id: "addr-locked".to_owned(),
                    keys: Keys(vec![key("locked", USER_KEY_SALTED)]),
                    ..Address::default()
                },
            ],
            &SaltedKeyPass::from_secret("password"),
        )
        .expect("unlock");
        assert_eq!(account.user.len(), 1);
        let mut ids: Vec<&str> = account.addresses.keys().map(String::as_str).collect();
        ids.sort_unstable();
        assert_eq!(ids, ["addr-legacy", "addr-token"]);
    }

    #[test]
    fn fails_without_user_or_address_keys() {
        // The user keys open, but there is no address to unlock.
        let error = unlock(
            &user(Keys(vec![key("user", USER_KEY_PASSWORD)])),
            &[],
            &SaltedKeyPass::from_secret("password"),
        )
        .expect_err("no addresses");
        assert!(error.to_string().contains("address keys"));
        // No user key opens.
        let salted = salted_password();
        let error = unlock(
            &user(Keys(vec![key("user", USER_KEY_PASSWORD)])),
            &[],
            &salted,
        )
        .expect_err("no user key");
        assert!(error.to_string().contains("user keys"));
        let error = unlock(
            &user(Keys(vec![key(USER_KEY_ID, USER_KEY_SALTED)])),
            &[Address {
                id: "addr".to_owned(),
                keys: Keys(vec![key("locked", USER_KEY_PASSWORD)]),
                ..Address::default()
            }],
            &salted,
        )
        .expect_err("no address key");
        assert!(error.to_string().contains("address keys"));
    }
}
