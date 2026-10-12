// SPDX-License-Identifier: AGPL-3.0-only
//! Sync API client: talks to the admin platform's `/api/tcb/state` endpoints
//! and the profile-scoped blob store under `/api/tcb/blobs`.
//!
//! Real reqwest-backed implementation: GET/POST with `Authorization: Bearer
//! {access_token}`, automatic refresh + retry once on 401, typed `CasConflict`
//! error on 409.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::sync::Arc;

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::auth::AdminAuth;
use crate::error::IdeError;

/// Base path of the admin platform's sync API (state, blobs, teams, agents).
/// Wire name, unchanged by the Mnemos IDE rename: the server is a separate repo
/// and older clients still call it.
pub(crate) const SYNC_API: &str = "/api/tcb";

/// Shape returned by `GET /api/tcb/state/:key` and `POST /api/tcb/state`.
/// snake_case fields match the admin backend response verbatim.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SyncStateResponse {
    pub key: String,
    pub value: Value,
    pub version: i64,
    pub synced_at: String,
}

/// Shape returned by `GET /api/tcb/state` — the bulk pull endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct PullAllResponse {
    pub states: Vec<SyncStateResponse>,
}

/// Wire shape for `POST /api/tcb/state` request body. `expected_version` lets
/// the server reject a push that races another client.
#[derive(Debug, Serialize)]
pub struct PushStateRequest<'a> {
    pub key: &'a str,
    pub value: &'a Value,
    pub expected_version: Option<i64>,
}

/// Wire shape for `POST /api/tcb/state/bulk` request body.
#[derive(Debug, Serialize)]
pub struct BulkPushRequest<'a> {
    pub states: Vec<BulkPushItem<'a>>,
}

#[derive(Debug, Serialize)]
pub struct BulkPushItem<'a> {
    pub key: &'a str,
    pub value: &'a Value,
}

/// Body of a 409 response — surfaces the server's current state so the caller
/// can reconcile (typically by re-pulling, merging, and re-pushing).
#[derive(Debug, Deserialize)]
struct ConflictBody {
    pub current_version: i64,
    pub current_value: Value,
}

/// Largest body the blob store accepts in one PUT (the server answers 413
/// above it). Checked before sending so an oversized upload fails fast.
pub const MAX_BLOB_BYTES: usize = 64 * 1024 * 1024;
/// Longest blob path (and list prefix) the server accepts, in bytes.
pub const MAX_BLOB_PATH_LEN: usize = 512;
/// How often one listing may start over after a cursor the server could no
/// longer continue (it answers 500 for an expired R2 continuation token).
const MAX_LIST_RESTARTS: usize = 2;
/// Hard stop for a server that keeps handing out cursors.
const MAX_LIST_PAGES: usize = 10_000;

/// One entry of `GET /api/tcb/blobs`. `path` is relative to the profile
/// root, `etag` is unquoted (the GET header keeps the quotes).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlobItem {
    pub path: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub etag: String,
    #[serde(default)]
    pub last_modified: String,
}

/// One page of a blob listing, as the server sends it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BlobPage {
    #[serde(default)]
    pub items: Vec<BlobItem>,
    #[serde(default)]
    pub next_cursor: Option<String>,
}

/// Start of the error every request returns once the server has refused this
/// client's refresh token. See [`is_session_rejected`].
const SESSION_REJECTED_PREFIX: &str = "admin session rejected: ";
/// Longest server reason kept for the UI. A proxy error page can be kilobytes
/// of HTML; the tooltip only needs the gist.
const MAX_REASON_CHARS: usize = 120;

/// What this client has learned about its admin session from the refresh
/// endpoint. Shared by every clone, like the token store.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionHealth {
    /// The server refused the refresh token (401/403), e.g. "Session not
    /// found or expired". Only a new sign-in brings the session back, so once
    /// set, every request fails without touching the network.
    pub rejected: Option<String>,
    /// The last token pair could not be written to the keyring. Sync keeps
    /// working on the in-memory pair, but the next app start loads the old,
    /// already rotated one and comes up signed out.
    pub save_failed: Option<String>,
    /// The sync server refused this build's protocol version (426 Upgrade
    /// Required, see [`crate::protocol`]). Only an updated Mnemos syncs
    /// again, so once set every request fails without touching the network.
    pub upgrade_required: Option<String>,
}

/// Reads the login saved for this process. See [`SyncClient::saved_login`].
type SavedLogin = Arc<dyn Fn() -> Result<AdminAuth, IdeError> + Send + Sync>;

/// `Clone` shares one token store: a refresh through any clone is seen by
/// all of them, so the manager and the transcript sync never race each other
/// into refreshing (and rotating away) the same refresh token twice.
#[derive(Clone)]
pub struct SyncClient {
    pub auth: Arc<Mutex<AdminAuth>>,
    pub http: reqwest::Client,
    health: Arc<std::sync::Mutex<SessionHealth>>,
    /// Where a refused refresh looks for a newer login before calling the
    /// session gone: the keyring, outside tests that pin what it holds.
    saved_login: SavedLogin,
    /// End-to-end encryption of personal state and transcripts, shared by
    /// every clone like the token store. See [`crate::e2e`].
    pub(crate) e2e: Arc<std::sync::RwLock<crate::e2e::E2eState>>,
}

impl SyncClient {
    pub fn new(auth: AdminAuth) -> Self {
        // Timeouts are load-bearing: pushes run inline in the sync event
        // loop, so a single hung request with no timeout stalls ALL future
        // pushes for the app's lifetime.
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(60))
            // X-Mnemos-Sync-Protocol on every request, the refresh included.
            .default_headers(crate::protocol::default_headers())
            .build()
            .unwrap_or_default();
        Self {
            auth: Arc::new(Mutex::new(auth)),
            http,
            health: Arc::new(std::sync::Mutex::new(SessionHealth::default())),
            saved_login: Arc::new(AdminAuth::load_from_keyring),
            e2e: Arc::new(std::sync::RwLock::new(crate::e2e::E2eState::default())),
        }
    }

    /// Tests: what a refused refresh finds saved, instead of the keyring,
    /// which every test in the process shares.
    #[cfg(test)]
    pub(crate) fn with_saved_login(
        mut self,
        saved_login: impl Fn() -> Result<AdminAuth, IdeError> + Send + Sync + 'static,
    ) -> Self {
        self.saved_login = Arc::new(saved_login);
        self
    }

    /// What the refresh endpoint has said about this session so far.
    pub fn session_health(&self) -> SessionHealth {
        self.health
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Record that the current token pair could not be saved to the keyring,
    /// for a pair this client did not refresh itself (the app's sign-in
    /// redirect). The next refresh that saves cleanly clears it.
    pub fn note_save_failed(&self, reason: String) {
        self.health
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .save_failed = Some(reason);
    }

    /// Fail fast once the session is known to be gone. Without this every
    /// push, poll and transcript upload would spend a refresh round-trip to
    /// hear the same 401 again, every few seconds, until the app restarts.
    fn ensure_session(&self) -> Result<(), IdeError> {
        let health = self.health.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(reason) = &health.upgrade_required {
            return Err(IdeError::Http {
                status: StatusCode::UPGRADE_REQUIRED,
                body: reason.clone(),
            });
        }
        match &health.rejected {
            Some(reason) => Err(IdeError::Auth(format!("{SESSION_REJECTED_PREFIX}{reason}"))),
            None => Ok(()),
        }
    }

    /// Refresh the token pair and record what the outcome says about the
    /// session. The caller holds the token store's lock.
    ///
    /// The refresh goes through `self.http` for its timeouts: every push,
    /// poll and transcript upload waits on that lock, and the manager's stop
    /// waits on them, so a refresh that never gets an answer must end as a
    /// timeout (retried on the backoff) instead of holding the lock forever.
    async fn refresh_locked(&self, auth: &mut AdminAuth) -> Result<(), IdeError> {
        // Another request may have learned the session is gone while this
        // one waited for the lock.
        self.ensure_session()?;
        let mut adopted_saved_login = false;
        loop {
            let e = match auth.refresh_with(&self.http).await {
                Ok(()) => {
                    // `refresh_with` returns Ok when only the keyring write failed;
                    // this is the one place that learns the saved login is
                    // stale.
                    let save_failed = AdminAuth::last_save_error();
                    if let Some(reason) = &save_failed {
                        tracing::warn!(
                            target: "mnemos_ide_sync::client",
                            "refreshed admin token could not be saved to the keyring; the next start will be signed out: {reason}",
                        );
                    }
                    self.health
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .save_failed = save_failed;
                    return Ok(());
                }
                Err(e) => e,
            };
            let Some(reason) = refresh_rejection(&e) else {
                return Err(e);
            };
            // Once only: the saved login gets one refresh of its own, and a
            // refusal of that one is final.
            if !adopted_saved_login && self.adopt_newer_saved_login(auth) {
                adopted_saved_login = true;
                if auth.bearer_token().is_ok() {
                    return Ok(());
                }
                continue;
            }
            tracing::warn!(
                target: "mnemos_ide_sync::client",
                "admin session rejected on refresh; sync stops until the next sign-in: {reason}",
            );
            self.health
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .rejected = Some(reason.clone());
            return Err(IdeError::Auth(format!("{SESSION_REJECTED_PREFIX}{reason}")));
        }
    }

    /// After a refused refresh: take the saved login instead, if it is for
    /// this server and holds a different refresh token than the one refused.
    ///
    /// Another client in this process may have refreshed first (the agents
    /// panel's suggest flow builds its own from the keyring). The server rotates the refresh
    /// token on every refresh, so that killed this client's copy while the
    /// saved login stayed good. Calling the session gone here would ask for a
    /// browser sign-in that a keyring read makes unnecessary.
    fn adopt_newer_saved_login(&self, auth: &mut AdminAuth) -> bool {
        let saved = match (self.saved_login)() {
            Ok(saved) => saved,
            Err(e) => {
                tracing::debug!(target: "mnemos_ide_sync::client", "no saved admin login to fall back on: {e}");
                return false;
            }
        };
        let refused = auth.tokens.as_ref().map(|t| t.refresh_token.as_str());
        let newer = saved.base_url == auth.base_url
            && saved
                .tokens
                .as_ref()
                .is_some_and(|t| Some(t.refresh_token.as_str()) != refused);
        if !newer {
            return false;
        }
        tracing::info!(
            target: "mnemos_ide_sync::client",
            "refresh token was rotated by another client; continuing with the saved login",
        );
        auth.tokens = saved.tokens;
        // This client now runs on exactly what the keyring holds, so an
        // earlier failed save no longer leaves the next start behind.
        self.health
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .save_failed = None;
        true
    }

    async fn base_url_and_token(&self) -> Result<(String, String), IdeError> {
        self.ensure_session()?;
        let mut auth = self.auth.lock().await;
        // Access tokens are short-lived; bearer_token() rejects an expired
        // one before any request is made, so the 401-triggered refresh in
        // `with_refresh` never gets a chance. Refresh proactively instead of
        // failing every push with NotAuthenticated once the token ages out.
        if auth.bearer_token().is_err() {
            self.refresh_locked(&mut auth).await?;
        }
        let token = auth.bearer_token()?.to_string();
        Ok((auth.base_url.clone(), token))
    }

    /// Perform `f` once; on 401, refresh tokens and retry once. A 426 from
    /// the sync server stops this client for good (see
    /// [`SessionHealth::upgrade_required`]).
    pub(crate) async fn with_refresh<F, Fut, T>(&self, f: F) -> Result<T, IdeError>
    where
        F: Fn(String, String) -> Fut,
        Fut: std::future::Future<Output = Result<T, IdeError>>,
    {
        let result = self.with_refresh_inner(f).await;
        if let Err(err) = &result {
            self.note_upgrade_required(err);
        }
        result
    }

    /// Record a protocol refusal, if `err` is one.
    fn note_upgrade_required(&self, err: &IdeError) {
        let IdeError::Http { status, body } = err else {
            return;
        };
        let Some(reason) = crate::protocol::upgrade_required_reason(*status, body) else {
            return;
        };
        let mut health = self.health.lock().unwrap_or_else(|e| e.into_inner());
        if health.upgrade_required.is_none() {
            tracing::warn!(
                target: "mnemos_ide_sync::client",
                "sync server refused this protocol version; sync stops until Mnemos is updated: {reason}",
            );
            health.upgrade_required = Some(reason);
        }
    }

    async fn with_refresh_inner<F, Fut, T>(&self, f: F) -> Result<T, IdeError>
    where
        F: Fn(String, String) -> Fut,
        Fut: std::future::Future<Output = Result<T, IdeError>>,
    {
        let (base_url, token) = self.base_url_and_token().await?;
        match f(base_url, token).await {
            Err(IdeError::Http { status, .. }) if status == StatusCode::UNAUTHORIZED => {
                tracing::debug!(target: "mnemos_ide_sync::client", "401 — refreshing and retrying once");
                {
                    let mut auth = self.auth.lock().await;
                    self.refresh_locked(&mut auth).await?;
                }
                let (base_url, token) = self.base_url_and_token().await?;
                f(base_url, token).await
            }
            other => other,
        }
    }

    /// `GET /api/tcb/state/:key` — fetch a single state blob.
    /// Returns `Ok(None)` for 404 (key not yet on server, caller treats as version=0).
    pub(crate) async fn get_state_raw(&self, key: &str) -> Result<Option<SyncStateResponse>, IdeError> {
        let key_owned = key.to_string();
        let http = self.http.clone();
        self.with_refresh(move |base_url, token| {
            let key = key_owned.clone();
            let http = http.clone();
            async move {
                let url = format!("{base_url}{SYNC_API}/state/{key}");
                let resp = http.get(&url).bearer_auth(&token).send().await?;
                let status = resp.status();
                if status == StatusCode::NOT_FOUND {
                    return Ok(None);
                }
                if status == StatusCode::UNAUTHORIZED {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(IdeError::Http { status, body });
                }
                if !status.is_success() {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(IdeError::Http { status, body });
                }
                let parsed: SyncStateResponse = resp.json().await?;
                Ok(Some(parsed))
            }
        })
        .await
    }

    /// `POST /api/tcb/state` — push a single state blob with optional CAS via
    /// `expected_version`. On 409 returns `Err(IdeError::CasConflict{...})`
    /// with the server's current state.
    pub(crate) async fn put_state_raw(
        &self,
        key: &str,
        value: &Value,
        expected_version: Option<i64>,
    ) -> Result<SyncStateResponse, IdeError> {
        let key_owned = key.to_string();
        let value_owned = value.clone();
        let http = self.http.clone();
        self.with_refresh(move |base_url, token| {
            let key = key_owned.clone();
            let value = value_owned.clone();
            let http = http.clone();
            async move {
                let url = format!("{base_url}{SYNC_API}/state");
                let body = PushStateRequest {
                    key: &key,
                    value: &value,
                    expected_version,
                };
                let resp = http
                    .post(&url)
                    .bearer_auth(&token)
                    .json(&body)
                    .send()
                    .await?;
                let status = resp.status();
                if status == StatusCode::CONFLICT {
                    let conflict: ConflictBody = resp.json().await?;
                    return Err(IdeError::CasConflict {
                        key,
                        current_version: conflict.current_version,
                        current_value: conflict.current_value,
                    });
                }
                if !status.is_success() {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(IdeError::Http { status, body });
                }
                let parsed: SyncStateResponse = resp.json().await?;
                Ok(parsed)
            }
        })
        .await
    }

    /// `DELETE /api/tcb/state/:key` — drop a state blob.
    pub(crate) async fn delete_state_raw(&self, key: &str) -> Result<(), IdeError> {
        let key_owned = key.to_string();
        let http = self.http.clone();
        self.with_refresh(move |base_url, token| {
            let key = key_owned.clone();
            let http = http.clone();
            async move {
                let url = format!("{base_url}{SYNC_API}/state/{key}");
                let resp = http.delete(&url).bearer_auth(&token).send().await?;
                let status = resp.status();
                if !status.is_success() && status != StatusCode::NOT_FOUND {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(IdeError::Http { status, body });
                }
                Ok(())
            }
        })
        .await
    }

    /// `DELETE /api/tcb/state/:key?expected_version=N`: drop a state value
    /// only while it is still at `expected_version`. `Ok(true)` when this
    /// call deleted it; `Ok(false)` when it was rewritten since (409) or is
    /// already gone (404). A server from before the conditional delete
    /// ignores the parameter and deletes whatever is there, as
    /// [`Self::delete_state_raw`] does.
    pub(crate) async fn delete_state_raw_at(
        &self,
        key: &str,
        expected_version: i64,
    ) -> Result<bool, IdeError> {
        let key_owned = key.to_string();
        let http = self.http.clone();
        self.with_refresh(move |base_url, token| {
            let key = key_owned.clone();
            let http = http.clone();
            async move {
                let url = conditional_delete_url(&base_url, &key, expected_version);
                let resp = http.delete(&url).bearer_auth(&token).send().await?;
                let status = resp.status();
                if status == StatusCode::CONFLICT || status == StatusCode::NOT_FOUND {
                    return Ok(false);
                }
                if !status.is_success() {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(IdeError::Http { status, body });
                }
                Ok(true)
            }
        })
        .await
    }

    /// The admin profile id in the access token this client holds, if any.
    pub async fn profile_id(&self) -> Option<String> {
        let token = {
            let auth = self.auth.lock().await;
            auth.tokens.as_ref().map(|t| t.access_token.clone())
        };
        token
            .as_deref()
            .and_then(crate::manager::extract_profile_id_from_jwt)
    }

    /// `GET /api/tcb/me`: what the server says about this account, which
    /// Settings > Sync uses to offer "Delete synced data" only where the
    /// server allows it. `Ok(None)` from a server without the route (404, or
    /// 405 from one that only has `DELETE /me`).
    pub async fn account_info(&self) -> Result<Option<AccountInfo>, IdeError> {
        let http = self.http.clone();
        self.with_refresh(move |base_url, token| {
            let http = http.clone();
            async move {
                let url = format!("{base_url}{SYNC_API}/me");
                let resp = http.get(&url).bearer_auth(&token).send().await?;
                let status = resp.status();
                if status == StatusCode::NOT_FOUND || status == StatusCode::METHOD_NOT_ALLOWED {
                    return Ok(None);
                }
                if !status.is_success() {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(IdeError::Http { status, body });
                }
                Ok(Some(resp.json::<AccountInfo>().await?))
            }
        })
        .await
    }

    /// `DELETE /api/tcb/me/data`: delete everything this account synced (all
    /// profile state keys and every blob under the profile's prefix). The
    /// account, its login and its team memberships stay.
    pub async fn delete_my_synced_data(&self) -> Result<(), IdeError> {
        self.delete_me("/me/data").await
    }

    /// `DELETE /api/tcb/me`: delete the synced data and the account itself.
    /// Team memberships are removed; a team other members still use is
    /// kept. The server refuses staff accounts.
    pub async fn delete_my_account(&self) -> Result<(), IdeError> {
        self.delete_me("/me").await
    }

    async fn delete_me(&self, path: &'static str) -> Result<(), IdeError> {
        let http = self.http.clone();
        self.with_refresh(move |base_url, token| {
            let http = http.clone();
            async move {
                let url = format!("{base_url}{SYNC_API}{path}");
                let resp = http.delete(&url).bearer_auth(&token).send().await?;
                let status = resp.status();
                if status == StatusCode::NOT_FOUND {
                    return Err(IdeError::Http {
                        status,
                        body: "the sync server does not support deletion yet".to_string(),
                    });
                }
                if !status.is_success() {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(IdeError::Http { status, body });
                }
                Ok(())
            }
        })
        .await
    }

    /// `GET /api/tcb/state` — fetch every state blob the user has on the server.
    /// Returns an empty Vec if the user has no state yet.
    pub(crate) async fn pull_all_raw(&self) -> Result<Vec<SyncStateResponse>, IdeError> {
        let http = self.http.clone();
        self.with_refresh(move |base_url, token| {
            let http = http.clone();
            async move {
                let url = format!("{base_url}{SYNC_API}/state");
                let resp = http.get(&url).bearer_auth(&token).send().await?;
                let status = resp.status();
                if status == StatusCode::UNAUTHORIZED {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(IdeError::Http { status, body });
                }
                if !status.is_success() {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(IdeError::Http { status, body });
                }
                let parsed: PullAllResponse = resp.json().await?;
                Ok(parsed.states)
            }
        })
        .await
    }

    /// `POST /api/tcb/state/bulk` — push many state blobs in one request.
    /// Plaintext only: refused for a personal key unless end-to-end
    /// encryption is known to be off (nothing in the app calls it today).
    pub async fn bulk_put(&self, items: HashMap<String, Value>) -> Result<(), IdeError> {
        if items.keys().any(|k| crate::crypto::is_encryptable_key(k))
            && self.e2e_status() != crate::crypto::E2eStatus::Off
        {
            return Err(IdeError::Other(
                "bulk_put sends plaintext; not used for personal keys unless end-to-end encryption is off".into(),
            ));
        }
        let items_owned = items;
        let http = self.http.clone();
        self.with_refresh(move |base_url, token| {
            let items = items_owned.clone();
            let http = http.clone();
            async move {
                let url = format!("{base_url}{SYNC_API}/state/bulk");
                let states: Vec<_> = items
                    .iter()
                    .map(|(k, v)| BulkPushItem {
                        key: k.as_str(),
                        value: v,
                    })
                    .collect();
                let body = BulkPushRequest { states };
                let resp = http
                    .post(&url)
                    .bearer_auth(&token)
                    .json(&body)
                    .send()
                    .await?;
                let status = resp.status();
                if !status.is_success() {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(IdeError::Http { status, body });
                }
                Ok(())
            }
        })
        .await
    }

    // ── Blob store (`/api/tcb/blobs`) ───────────────────────────────────────
    //
    // Paths are validated here with the server's rules before any request:
    // the allowed charset needs no percent-encoding, so the path goes into
    // the URL verbatim, and a bad path fails fast instead of as a 400.

    /// `PUT /api/tcb/blobs/{path}`: store `bytes` at `path`, overwriting.
    pub(crate) async fn put_blob_raw(&self, path: &str, bytes: Vec<u8>) -> Result<(), IdeError> {
        validate_blob_path(path)?;
        if bytes.len() > MAX_BLOB_BYTES {
            return Err(IdeError::Other(format!(
                "blob {path} is {} bytes; the server accepts at most {MAX_BLOB_BYTES}",
                bytes.len()
            )));
        }
        let path_owned = path.to_string();
        let http = self.http.clone();
        self.with_refresh(move |base_url, token| {
            let url = format!("{base_url}{SYNC_API}/blobs/{path_owned}");
            // Cloned per attempt: the 401 retry needs the body a second time.
            let body = bytes.clone();
            let http = http.clone();
            async move {
                let resp = http
                    .put(&url)
                    .bearer_auth(&token)
                    .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                    .body(body)
                    .send()
                    .await?;
                let status = resp.status();
                if !status.is_success() {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(IdeError::Http { status, body });
                }
                Ok(())
            }
        })
        .await
    }

    /// `GET /api/tcb/blobs/{path}`: the blob's bytes, `Ok(None)` on 404.
    pub(crate) async fn get_blob_raw(&self, path: &str) -> Result<Option<Vec<u8>>, IdeError> {
        validate_blob_path(path)?;
        let path_owned = path.to_string();
        let http = self.http.clone();
        self.with_refresh(move |base_url, token| {
            let url = format!("{base_url}{SYNC_API}/blobs/{path_owned}");
            let http = http.clone();
            async move {
                let resp = http.get(&url).bearer_auth(&token).send().await?;
                let status = resp.status();
                if status == StatusCode::NOT_FOUND {
                    return Ok(None);
                }
                if !status.is_success() {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(IdeError::Http { status, body });
                }
                Ok(Some(resp.bytes().await?.to_vec()))
            }
        })
        .await
    }

    /// `GET /api/tcb/blobs?prefix=`: every blob under `prefix`, following
    /// `nextCursor` to the end. A cursor the server can no longer continue
    /// (5xx) restarts the listing from the top rather than failing it.
    pub(crate) async fn list_blobs_raw(&self, prefix: &str) -> Result<Vec<BlobItem>, IdeError> {
        validate_blob_prefix(prefix)?;
        collect_blob_pages(|cursor| self.list_blob_page(prefix, cursor)).await
    }

    /// One page of [`Self::list_blobs`].
    async fn list_blob_page(
        &self,
        prefix: &str,
        cursor: Option<String>,
    ) -> Result<BlobPage, IdeError> {
        let prefix = prefix.to_string();
        let http = self.http.clone();
        self.with_refresh(move |base_url, token| {
            let url = format!("{base_url}{SYNC_API}/blobs");
            let http = http.clone();
            // `query` percent-encodes: R2 continuation tokens carry `+ / =`.
            let mut query: Vec<(&'static str, String)> = vec![("prefix", prefix.clone())];
            if let Some(c) = &cursor {
                query.push(("cursor", c.clone()));
            }
            async move {
                let resp = http
                    .get(&url)
                    .bearer_auth(&token)
                    .query(&query)
                    .send()
                    .await?;
                let status = resp.status();
                if !status.is_success() {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(IdeError::Http { status, body });
                }
                Ok(resp.json::<BlobPage>().await?)
            }
        })
        .await
    }

    /// `DELETE /api/tcb/blobs/{path}`: idempotent; a missing blob is fine.
    pub(crate) async fn delete_blob_raw(&self, path: &str) -> Result<(), IdeError> {
        validate_blob_path(path)?;
        let path_owned = path.to_string();
        let http = self.http.clone();
        self.with_refresh(move |base_url, token| {
            let url = format!("{base_url}{SYNC_API}/blobs/{path_owned}");
            let http = http.clone();
            async move {
                let resp = http.delete(&url).bearer_auth(&token).send().await?;
                let status = resp.status();
                if !status.is_success() && status != StatusCode::NOT_FOUND {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(IdeError::Http { status, body });
                }
                Ok(())
            }
        })
        .await
    }
}

/// `GET /api/tcb/me`, see [`SyncClient::account_info`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountInfo {
    /// `mnemos` for an account created from Mnemos, `admin` for a staff
    /// account of the server's own admin platform.
    #[serde(default)]
    pub account_source: String,
    /// Whether `DELETE /me/data` and `DELETE /me` accept this account. Staff
    /// accounts are refused (their data is deleted by an operator).
    #[serde(default)]
    pub can_delete_data: bool,
}

/// The URL of a conditional `DELETE /state/:key` (see
/// [`SyncClient::delete_state_raw_at`]). `key` is already path-escaped.
fn conditional_delete_url(base_url: &str, key: &str, expected_version: i64) -> String {
    format!("{base_url}{SYNC_API}/state/{key}?expected_version={expected_version}")
}

/// The HTTP status of a failed `AdminAuth::refresh_with`, which reports it as
/// `Auth("refresh failed: {status} {body}")` rather than as `Http`.
pub(crate) fn refresh_failure_status(err: &IdeError) -> Option<StatusCode> {
    let IdeError::Auth(msg) = err else {
        return None;
    };
    let code = msg
        .strip_prefix("refresh failed: ")?
        .split_whitespace()
        .next()?
        .parse::<u16>()
        .ok()?;
    StatusCode::from_u16(code).ok()
}

/// The server's reason when a refresh failed because it refused the session
/// (401/403), `None` for any other failure. Offline, DNS and 5xx are not a
/// verdict on the session: those keep retrying as before.
///
/// Only the admin backend's own answer counts. Every error it sends is
/// `{"error": "..."}`, e.g. "Session not found or expired". A 401/403 in any
/// other shape came from something in between (a filtering proxy's block
/// page, a firewall challenge) and says nothing about the session, which may
/// work again once the machine is off that network. See
/// [`refresh_intercepted`].
fn refresh_rejection(err: &IdeError) -> Option<String> {
    let status = refresh_failure_status(err)?;
    if status != StatusCode::UNAUTHORIZED && status != StatusCode::FORBIDDEN {
        return None;
    }
    let IdeError::Auth(msg) = err else {
        return None;
    };
    // After "refresh failed: 401 Unauthorized " comes the body.
    let body = msg
        .strip_prefix("refresh failed: ")
        .and_then(|rest| rest.strip_prefix(&status.to_string()))
        .unwrap_or_default()
        .trim();
    let reason = serde_json::from_str::<Value>(body)
        .ok()?
        .get("error")?
        .as_str()?
        .to_owned();
    let reason = if reason.trim().is_empty() {
        status.to_string()
    } else {
        reason
    };
    Some(reason.chars().take(MAX_REASON_CHARS).collect())
}

/// A refresh answered 401/403 by something other than the admin backend
/// (see [`refresh_rejection`]). Retried on the backoff like a 5xx, so sync
/// resumes by itself once the machine leaves the network that blocked it.
pub(crate) fn refresh_intercepted(err: &IdeError) -> bool {
    refresh_failure_status(err)
        .is_some_and(|s| s == StatusCode::UNAUTHORIZED || s == StatusCode::FORBIDDEN)
        && refresh_rejection(err).is_none()
}

/// Whether `err` means the sync server refused this build's protocol version
/// (426), live or remembered from an earlier request.
pub fn is_upgrade_required(err: &IdeError) -> bool {
    matches!(err, IdeError::Http { status, .. } if *status == StatusCode::UPGRADE_REQUIRED)
}

/// Whether `err` means the admin session is gone for good (the server refused
/// the refresh token), as opposed to a failure worth retrying.
pub fn is_session_rejected(err: &IdeError) -> bool {
    matches!(err, IdeError::Auth(msg) if msg.starts_with(SESSION_REJECTED_PREFIX))
}

fn blob_path_chars_ok(s: &str) -> bool {
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
}

/// The server's rule for a blob path: `[A-Za-z0-9._-/]`, at most 512 bytes,
/// no leading `/`, no `..` anywhere, no empty or `.` segments.
pub fn validate_blob_path(path: &str) -> Result<(), IdeError> {
    let bad = |why: &str| {
        Err(IdeError::Other(format!(
            "invalid blob path {path:?}: {why}"
        )))
    };
    if path.is_empty() {
        return bad("empty");
    }
    if path.len() > MAX_BLOB_PATH_LEN {
        return bad("too long");
    }
    if !blob_path_chars_ok(path) {
        return bad("only A-Z a-z 0-9 . _ - / are allowed");
    }
    if path.starts_with('/') || path.contains("..") {
        return bad("leading '/' or '..'");
    }
    if path.split('/').any(|seg| seg.is_empty() || seg == ".") {
        return bad("empty or '.' segment");
    }
    Ok(())
}

/// A list prefix: like a path, but may be empty and may end in `/`.
pub fn validate_blob_prefix(prefix: &str) -> Result<(), IdeError> {
    if prefix.is_empty() {
        return Ok(());
    }
    let bad = |why: &str| {
        Err(IdeError::Other(format!(
            "invalid blob prefix {prefix:?}: {why}"
        )))
    };
    if prefix.len() > MAX_BLOB_PATH_LEN {
        return bad("too long");
    }
    if !blob_path_chars_ok(prefix) {
        return bad("only A-Z a-z 0-9 . _ - / are allowed");
    }
    if prefix.starts_with('/') || prefix.contains("..") {
        return bad("leading '/' or '..'");
    }
    let mut segments: Vec<&str> = prefix.split('/').collect();
    segments.pop();
    if segments.iter().any(|seg| seg.is_empty() || *seg == ".") {
        return bad("empty or '.' segment");
    }
    Ok(())
}

/// Drive a paged listing to the end. Split out from [`SyncClient::list_blobs`]
/// so the cursor and restart handling is testable without a server.
///
/// A restart discards what the aborted pass collected: the result is always
/// one complete pass, never a mix of two points in time.
pub(crate) async fn collect_blob_pages<F, Fut>(mut fetch: F) -> Result<Vec<BlobItem>, IdeError>
where
    F: FnMut(Option<String>) -> Fut,
    Fut: Future<Output = Result<BlobPage, IdeError>>,
{
    let mut items: BTreeMap<String, BlobItem> = BTreeMap::new();
    let mut cursor: Option<String> = None;
    let mut restarts = 0;
    let mut pages = 0;
    loop {
        match fetch(cursor.clone()).await {
            Ok(page) => {
                for item in page.items {
                    items.insert(item.path.clone(), item);
                }
                match page.next_cursor.filter(|c| !c.is_empty()) {
                    Some(next) => cursor = Some(next),
                    None => break,
                }
                pages += 1;
                if pages > MAX_LIST_PAGES {
                    return Err(IdeError::Other(format!(
                        "blob listing still had more after {MAX_LIST_PAGES} pages"
                    )));
                }
            }
            Err(IdeError::Http { status, body })
                if cursor.is_some() && status.is_server_error() && restarts < MAX_LIST_RESTARTS =>
            {
                restarts += 1;
                tracing::debug!(
                    target: "mnemos_ide_sync::client",
                    %status,
                    body,
                    restarts,
                    "blob listing cursor failed; restarting the listing"
                );
                items.clear();
                cursor = None;
                pages = 0;
            }
            Err(e) => return Err(e),
        }
    }
    Ok(items.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unauthenticated_get_state_errors() {
        let c = SyncClient::new(AdminAuth::with_base_url("https://admin.example.test"));
        let r = c.get_state("projects").await;
        assert!(matches!(r, Err(IdeError::NotAuthenticated)));
    }

    #[tokio::test]
    async fn unauthenticated_put_state_errors() {
        let c = SyncClient::new(AdminAuth::with_base_url("https://admin.example.test"));
        let r = c
            .put_state("projects", &serde_json::json!({"a": 1}), None)
            .await;
        assert!(matches!(r, Err(IdeError::NotAuthenticated)));
    }

    #[test]
    fn conditional_delete_carries_the_expected_version() {
        assert_eq!(
            conditional_delete_url("https://a.test", "claude-md", 7),
            "https://a.test/api/tcb/state/claude-md?expected_version=7"
        );
    }

    #[test]
    fn account_info_parses_and_defaults_to_not_deletable() {
        let info: AccountInfo = serde_json::from_value(
            serde_json::json!({"accountSource": "mnemos", "canDeleteData": true}),
        )
        .unwrap();
        assert!(info.can_delete_data);
        assert_eq!(info.account_source, "mnemos");
        // A field the server leaves out never offers a deletion.
        let info: AccountInfo = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(!info.can_delete_data);
    }

    #[tokio::test]
    async fn clones_share_one_token_store() {
        let a = SyncClient::new(AdminAuth::with_base_url("https://admin.example.test"));
        let b = a.clone();
        assert!(Arc::ptr_eq(&a.auth, &b.auth));
        let r = b.get_blob("claude-sessions/s/m.meta.json").await;
        assert!(matches!(r, Err(IdeError::NotAuthenticated)));
    }

    #[tokio::test]
    async fn blob_paths_are_checked_before_any_request() {
        // Unauthenticated, so a request would fail with NotAuthenticated;
        // an `Other` proves validation ran first.
        let c = SyncClient::new(AdminAuth::with_base_url("https://admin.example.test"));
        for bad in [
            "", "/a", "a/../b", "a//b", "a/./b", "a b", "a\\b", "C:/x", "a/",
        ] {
            assert!(
                matches!(c.put_blob(bad, vec![1]).await, Err(IdeError::Other(_))),
                "{bad:?}"
            );
            assert!(
                matches!(c.get_blob(bad).await, Err(IdeError::Other(_))),
                "{bad:?}"
            );
            assert!(
                matches!(c.delete_blob(bad).await, Err(IdeError::Other(_))),
                "{bad:?}"
            );
        }
        assert!(matches!(
            c.list_blobs("../x").await,
            Err(IdeError::Other(_))
        ));
        assert!(matches!(
            c.put_blob("a", vec![0; MAX_BLOB_BYTES + 1]).await,
            Err(IdeError::Other(_))
        ));
        assert!(validate_blob_path(
            "claude-sessions/0b6f3c1e-8a3b-4f7e-9d2a-1c2d3e4f5a6b/0123abcd-000001.jsonl.gz"
        )
        .is_ok());
        assert!(validate_blob_prefix("claude-sessions/").is_ok());
        assert!(validate_blob_prefix("").is_ok());
        assert!(validate_blob_prefix("a//").is_err());
    }

    fn refresh_failed(status: StatusCode, body: &str) -> IdeError {
        // The shape `AdminAuth::refresh` reports a non-2xx answer in; the
        // manager test `a_refused_refresh_signs_out_and_stops_all_requests`
        // drives the real one against a fake server.
        IdeError::Auth(format!("refresh failed: {status} {body}"))
    }

    #[test]
    fn a_refused_refresh_is_a_rejected_session_with_the_servers_reason() {
        let err = refresh_failed(
            StatusCode::UNAUTHORIZED,
            r#"{"error":"Session not found or expired"}"#,
        );
        assert_eq!(refresh_failure_status(&err), Some(StatusCode::UNAUTHORIZED));
        assert_eq!(
            refresh_rejection(&err).as_deref(),
            Some("Session not found or expired")
        );
        assert!(!refresh_intercepted(&err));
        let inactive = refresh_failed(StatusCode::FORBIDDEN, r#"{"error":"Account is inactive"}"#);
        assert_eq!(
            refresh_rejection(&inactive).as_deref(),
            Some("Account is inactive")
        );
        // A long reason is cut short for the UI.
        let long = format!(r#"{{"error":"{}"}}"#, "x".repeat(500));
        let reason = refresh_rejection(&refresh_failed(StatusCode::UNAUTHORIZED, &long)).unwrap();
        assert_eq!(reason.chars().count(), MAX_REASON_CHARS);
        // The backend's shape with nothing in it: the status.
        assert_eq!(
            refresh_rejection(&refresh_failed(StatusCode::UNAUTHORIZED, r#"{"error":""}"#))
                .as_deref(),
            Some("401 Unauthorized")
        );
    }

    #[test]
    fn a_401_or_403_the_backend_did_not_send_is_no_verdict_on_the_session() {
        // A filtering proxy's block page, a firewall challenge, an empty
        // answer, JSON of some other shape: none is the admin backend's
        // `{"error": "..."}`, so none ends the session.
        for (status, body) in [
            (
                StatusCode::FORBIDDEN,
                "<html><body>Blocked by policy</body></html>",
            ),
            (StatusCode::FORBIDDEN, "Forbidden"),
            (StatusCode::UNAUTHORIZED, ""),
            (StatusCode::UNAUTHORIZED, r#"{"message":"Unauthorized"}"#),
            (StatusCode::FORBIDDEN, r#""blocked""#),
        ] {
            let err = refresh_failed(status, body);
            assert_eq!(refresh_rejection(&err), None, "{status} {body}");
            assert!(refresh_intercepted(&err), "{status} {body}");
        }
        // Other statuses are not interceptions either; they have their own
        // handling (5xx and 429 retry, the rest are errors).
        assert!(!refresh_intercepted(&refresh_failed(
            StatusCode::BAD_GATEWAY,
            "<html>"
        )));
        assert!(!refresh_intercepted(&IdeError::NotAuthenticated));
    }

    #[test]
    fn only_401_and_403_on_refresh_reject_the_session() {
        for status in [
            StatusCode::BAD_GATEWAY,
            StatusCode::SERVICE_UNAVAILABLE,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::NOT_FOUND,
        ] {
            let err = refresh_failed(status, "{}");
            assert_eq!(refresh_failure_status(&err), Some(status));
            assert_eq!(refresh_rejection(&err), None, "{status}");
        }
        assert_eq!(refresh_rejection(&IdeError::NotAuthenticated), None);
        assert_eq!(
            refresh_rejection(&IdeError::Auth("login failed: 401".into())),
            None
        );
        // A 401 on an ordinary request is retried with a refresh; only the
        // refresh's own verdict counts.
        let plain = IdeError::Http {
            status: StatusCode::UNAUTHORIZED,
            body: String::new(),
        };
        assert_eq!(refresh_rejection(&plain), None);
        assert!(!is_session_rejected(&plain));
    }

    #[tokio::test]
    async fn a_rejected_session_fails_every_request_without_the_network() {
        let mut auth = AdminAuth::from_tokens(
            "https://admin.example.test",
            crate::auth::AdminTokens {
                access_token: "AT".to_string(),
                refresh_token: "RT".to_string(),
                expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
            },
        );
        // Nothing listens here: a request that went out would come back as a
        // connect error, not as the rejection.
        auth.base_url = "http://127.0.0.1:1".to_string();
        let c = SyncClient::new(auth);
        c.health.lock().unwrap().rejected = Some("Session not found or expired".into());
        let clone = c.clone();
        for err in [
            c.get_state("projects").await.unwrap_err(),
            clone.pull_all().await.unwrap_err(),
            clone.put_blob("a/b", vec![1]).await.unwrap_err(),
        ] {
            assert!(is_session_rejected(&err), "{err}");
            assert!(
                err.to_string().contains("Session not found or expired"),
                "{err}"
            );
        }
    }

    #[tokio::test]
    async fn a_refresh_that_never_gets_an_answer_times_out_and_keeps_the_session() {
        // Accepts every connection and never answers. The sockets are kept
        // open: a dropped one reads as EOF, a fast error rather than the
        // silence this is about.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept().await {
                held.push(stream);
            }
        });

        // Expired, so the first request has to refresh before it can send.
        let mut auth = AdminAuth::from_tokens(
            "https://admin.example.test",
            crate::auth::AdminTokens {
                access_token: "AT".to_string(),
                refresh_token: "RT".to_string(),
                expires_at: chrono::Utc::now() - chrono::Duration::hours(1),
            },
        );
        auth.base_url = format!("http://{addr}");
        let mut c = SyncClient::new(auth);
        c.http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(300))
            .no_proxy()
            .build()
            .unwrap();

        let started = instant::Instant::now();
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(10), c.get_state("projects"))
                .await
                .expect("a refresh with no answer must time out, not hang");
        let elapsed = started.elapsed();
        server.abort();

        let err = result.unwrap_err();
        match &err {
            IdeError::Network(e) => {
                assert!(e.is_timeout(), "{err}");
                assert_eq!(
                    e.url().map(|u| u.path()),
                    Some("/api/auth/refresh"),
                    "{err}"
                );
            }
            other => panic!("expected a network timeout, got {other}"),
        }
        assert!(elapsed < std::time::Duration::from_secs(5), "{elapsed:?}");
        // Retried on the backoff, and no verdict on the session.
        assert!(crate::manager::is_transient(&err), "{err}");
        assert_eq!(c.session_health(), SessionHealth::default());
        assert_eq!(
            crate::manager::SyncAuthState::from_health(&c.session_health()),
            crate::manager::SyncAuthState::SignedIn
        );
        // The token store is free again for every other request.
        assert!(c.auth.try_lock().is_ok());
    }

    #[test]
    fn a_save_failure_noted_by_the_app_is_shared_by_every_clone() {
        let c = SyncClient::new(AdminAuth::with_base_url("https://admin.example.test"));
        let clone = c.clone();
        assert_eq!(c.session_health(), SessionHealth::default());
        clone.note_save_failed("access denied".into());
        assert_eq!(
            c.session_health().save_failed.as_deref(),
            Some("access denied")
        );
        assert_eq!(c.session_health().rejected, None);
    }

    fn item(path: &str) -> BlobItem {
        BlobItem {
            path: path.to_string(),
            size: 1,
            etag: "e".to_string(),
            last_modified: "2026-09-24T07:00:00.000Z".to_string(),
        }
    }

    fn page(paths: &[&str], next: Option<&str>) -> Result<BlobPage, IdeError> {
        Ok(BlobPage {
            items: paths.iter().map(|p| item(p)).collect(),
            next_cursor: next.map(str::to_string),
        })
    }

    fn server_error() -> Result<BlobPage, IdeError> {
        Err(IdeError::Http {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            body: "Failed to list blobs".to_string(),
        })
    }

    #[test]
    fn listing_page_parses_the_server_shape() {
        let body = r#"{"items":[{"path":"a/b","size":3,"etag":"x1","lastModified":"2026-09-24T07:00:00.000Z"}],"nextCursor":null}"#;
        let parsed: BlobPage = serde_json::from_str(body).unwrap();
        assert_eq!(
            parsed.items,
            vec![BlobItem {
                size: 3,
                etag: "x1".into(),
                ..item("a/b")
            }]
        );
        assert!(parsed.next_cursor.is_none());
    }

    #[tokio::test]
    async fn listing_follows_cursors_to_the_end() {
        let seen = std::sync::Mutex::new(Vec::new());
        let items = collect_blob_pages(|cursor| {
            seen.lock().unwrap().push(cursor.clone());
            let out = match cursor.as_deref() {
                None => page(&["a", "b"], Some("t+/= 1")),
                Some("t+/= 1") => page(&["c"], Some("t2")),
                Some("t2") => page(&["d"], None),
                other => panic!("unexpected cursor {other:?}"),
            };
            async move { out }
        })
        .await
        .unwrap();
        let paths: Vec<&str> = items.iter().map(|i| i.path.as_str()).collect();
        assert_eq!(paths, ["a", "b", "c", "d"]);
        assert_eq!(
            *seen.lock().unwrap(),
            vec![None, Some("t+/= 1".to_string()), Some("t2".to_string())]
        );
    }

    #[tokio::test]
    async fn stale_cursor_restarts_the_listing_once_more() {
        let calls = std::sync::Mutex::new(0usize);
        let items = collect_blob_pages(|cursor| {
            let n = {
                let mut c = calls.lock().unwrap();
                *c += 1;
                *c
            };
            // First pass: page 1 then a 500 on the cursor. Second pass: the
            // store changed in between ("stale" is gone), and it completes.
            let out = match (n, cursor.as_deref()) {
                (1, None) => page(&["stale", "x"], Some("c1")),
                (2, Some("c1")) => server_error(),
                (3, None) => page(&["x"], Some("c2")),
                (4, Some("c2")) => page(&["y"], None),
                other => panic!("unexpected call {other:?}"),
            };
            async move { out }
        })
        .await
        .unwrap();
        let paths: Vec<&str> = items.iter().map(|i| i.path.as_str()).collect();
        assert_eq!(
            paths,
            ["x", "y"],
            "the aborted pass must not leak into the result"
        );
    }

    #[tokio::test]
    async fn listing_gives_up_after_repeated_cursor_failures() {
        let r = collect_blob_pages(|cursor| {
            let out = match cursor {
                None => page(&["a"], Some("c")),
                Some(_) => server_error(),
            };
            async move { out }
        })
        .await;
        assert!(matches!(r, Err(IdeError::Http { .. })));

        // A failure on the first page has no cursor to blame: no restart.
        let calls = std::sync::Mutex::new(0usize);
        let r = collect_blob_pages(|_| {
            *calls.lock().unwrap() += 1;
            async { server_error() }
        })
        .await;
        assert!(r.is_err());
        assert_eq!(*calls.lock().unwrap(), 1);
    }
}
