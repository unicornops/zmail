// Ported from go-proton-api `client.go` at
// 8a62023d3e38cb29749bcfc01bf44eafda6cec82
// (https://github.com/ProtonMail/go-proton-api).
// Copyright (c) 2020 James Houlahan, Copyright (c) 2022 Proton AG.
// Licensed under the MIT License; see crates/zmail-proton-api/LICENSE-MIT.
// Modified by UnicornOps on 2026-10-20: rewritten in Rust. A request that
// gets 401 refreshes the session once and is retried once, as upstream.
// Concurrent 401s share one refresh: a task skips the refresh when another
// has already replaced the token it used. Per-client resty hooks and client
// IDs are not ported.

//! An authenticated API client for one session.

use std::{
    fmt,
    sync::{Arc, Mutex, MutexGuard},
};

use serde::de::DeserializeOwned;

use crate::{
    manager::{AuthHeaders, Manager, Request, Response},
    manager_auth::Auth,
    response::Error,
};

type AuthHandler = Arc<dyn Fn(&Auth) + Send + Sync>;
type DeauthHandler = Arc<dyn Fn() + Send + Sync>;

/// A Proton session: the UID, the access token and the refresh token.
#[derive(Clone, Default)]
struct Session {
    uid: String,
    access_token: String,
    refresh_token: String,
}

#[derive(Default)]
struct Handlers {
    auth: Vec<AuthHandler>,
    deauth: Vec<DeauthHandler>,
    deauthed: bool,
}

/// An API client bound to one session (go-proton-api's `Client`).
pub struct Client {
    manager: Manager,
    session: Mutex<Session>,
    handlers: Mutex<Handlers>,
    /// Serialises token refreshes (go-proton-api's `authLock`).
    refresh: tokio::sync::Mutex<()>,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("uid", &self.uid())
            .finish_non_exhaustive()
    }
}

impl Client {
    pub(crate) fn new(
        manager: Manager,
        uid: &str,
        access_token: &str,
        refresh_token: &str,
    ) -> Self {
        Self {
            manager,
            session: Mutex::new(Session {
                uid: uid.to_owned(),
                access_token: access_token.to_owned(),
                refresh_token: refresh_token.to_owned(),
            }),
            handlers: Mutex::new(Handlers::default()),
            refresh: tokio::sync::Mutex::new(()),
        }
    }

    /// The manager this client sends through.
    pub fn manager(&self) -> &Manager {
        &self.manager
    }

    /// The session UID.
    pub fn uid(&self) -> String {
        lock(&self.session).uid.clone()
    }

    /// Calls `handler` with the new tokens whenever the session is
    /// refreshed, so the caller can store the new refresh token.
    pub fn add_auth_handler(&self, handler: impl Fn(&Auth) + Send + Sync + 'static) {
        lock(&self.handlers).auth.push(Arc::new(handler));
    }

    /// Calls `handler` once when Proton rejects the refresh token, which
    /// means the user has to sign in again.
    pub fn add_deauth_handler(&self, handler: impl Fn() + Send + Sync + 'static) {
        lock(&self.handlers).deauth.push(Arc::new(handler));
    }

    /// Forgets the tokens and handlers (`Close`).
    pub fn close(&self) {
        *lock(&self.session) = Session::default();
        let mut handlers = lock(&self.handlers);
        handlers.auth.clear();
        handlers.deauth.clear();
    }

    /// Sends a request with the session headers and decodes the JSON body.
    pub(crate) async fn json<T: DeserializeOwned>(&self, request: &Request) -> Result<T, Error> {
        self.send(request).await?.json()
    }

    /// Sends a request with the session headers. On 401 the session is
    /// refreshed once and the request is retried once (`doRes`).
    pub(crate) async fn send(&self, request: &Request) -> Result<Response, Error> {
        let session = lock(&self.session).clone();
        match self.send_with(request, &session).await {
            Err(error) if error.status() == Some(401) => {
                self.refresh_after_401(&session.access_token).await?;
                let session = lock(&self.session).clone();
                self.send_with(request, &session).await
            }
            outcome => outcome,
        }
    }

    async fn send_with(&self, request: &Request, session: &Session) -> Result<Response, Error> {
        let auth = AuthHeaders {
            uid: &session.uid,
            access_token: &session.access_token,
        };
        self.manager.send(request, Some(&auth)).await
    }

    /// Refreshes the session unless another request already did since
    /// `stale_access_token` was used (`authRefresh`).
    async fn refresh_after_401(&self, stale_access_token: &str) -> Result<(), Error> {
        let _guard = self.refresh.lock().await;
        let session = lock(&self.session).clone();
        if session.access_token != stale_access_token {
            return Ok(());
        }
        let auth = match self
            .manager
            .auth_refresh(&session.uid, &session.refresh_token, &session.access_token)
            .await
        {
            Ok(auth) => auth,
            Err(error) => {
                // 400 and 422 mean Proton rejected the refresh token: the
                // session is over. Other failures (409, 429, 5xx, network)
                // may succeed later.
                if matches!(error.status(), Some(400 | 422)) {
                    self.deauth();
                    return Err(Error::Deauthenticated(Box::new(error)));
                }
                return Err(error);
            }
        };
        {
            let mut current = lock(&self.session);
            current.access_token = auth.access_token.clone();
            current.refresh_token = auth.refresh_token.clone();
        }
        let handlers = lock(&self.handlers).auth.clone();
        for handler in handlers {
            handler(&auth);
        }
        Ok(())
    }

    /// Runs the de-auth handlers once (`deauthOnce`).
    fn deauth(&self) {
        let handlers = {
            let mut handlers = lock(&self.handlers);
            if handlers.deauthed {
                return;
            }
            handlers.deauthed = true;
            handlers.deauth.clone()
        };
        for handler in handlers {
            handler();
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
