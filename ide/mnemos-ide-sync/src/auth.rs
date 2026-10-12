// SPDX-License-Identifier: AGPL-3.0-only
//! Admin platform auth wrapper.
//!
//! Owns the `AdminAuth` struct + keyring round-trip and the real
//! `login()` / `refresh()` HTTP calls against `{base_url}/api/auth/*`.
//! [`SyncClient`](crate::client::SyncClient) refreshes through
//! [`AdminAuth::refresh_with`] on its own client, so the refresh gets the
//! same timeouts as every other sync request.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

use chrono::{DateTime, Duration, Utc};
use keyring::credential::CredentialApi;
use keyring::Entry;
use serde::{Deserialize, Serialize};

use crate::error::IdeError;

/// Keyring service of the default (daily-driver) install. Unchanged from
/// before isolated builds got their own entry, so an upgraded install still
/// finds the login it saved.
///
/// The `tcbv2*` service names are on-disk (keychain) names and deliberately
/// kept through the Mnemos IDE rename: the admin server rotates the refresh
/// token on every refresh, so a copy under a new name would fork it, and an
/// older build or a rollback on the same machine would end up signed out.
/// Never rename them.
const DEFAULT_KEYRING_SERVICE: &str = "tcbv2";
/// Keyring service of a process whose Mnemos IDE data dir is redirected without a
/// data profile. See [`keyring_service`].
const ISOLATED_KEYRING_SERVICE: &str = "tcbv2-isolated";
const KEYRING_USER: &str = "admin-token";

/// Keyring service this process keeps its admin login under. The user is
/// always `admin-token`, so the Windows Credential Manager target is
/// `admin-token.<service>` and the macOS keychain item's service is
/// `<service>`.
///
/// - `tcbv2`: the default install.
/// - `tcbv2-<profile>`: `MNEMOS_DATA_PROFILE` is set and non-empty
///   ([`mnemos_ide_core::paths::data_profile`]). Every char outside `[A-Za-z0-9_-]`
///   becomes `_`, which keeps the name a plain token every OS store accepts
///   and a person can find in Credential Manager or Keychain Access. Two
///   profiles that differ only in such chars share an entry.
/// - `tcbv2-isolated`: the Mnemos IDE data dir is redirected (`MNEMOS_DATA_DIR`, the
///   legacy `TCB_DATA_DIR`, or [`mnemos_ide_core::paths::override_data_dir`]) and there is no data profile.
///
/// Every isolated name carries a suffix and the default one has none, so a
/// dev build or a test can never read, overwrite or delete the daily-driver
/// login. The flip side: an isolated build starts logged out and keeps its
/// own login from then on.
///
/// Evaluated on every call rather than cached, so a process that redirects
/// its data dir after start (app tests do) moves to the isolated entry from
/// that point instead of staying on the default one.
pub fn keyring_service() -> String {
    keyring_service_for(
        mnemos_ide_core::paths::data_profile().as_deref(),
        mnemos_ide_core::paths::data_dir_is_redirected(),
    )
}

/// The naming rules of [`keyring_service`], apart from the process state they
/// read, so tests can cover every combination without mutating env vars or
/// the one-shot data dir override.
fn keyring_service_for(profile: Option<&str>, data_dir_redirected: bool) -> String {
    match profile.filter(|p| !p.is_empty()) {
        Some(profile) => {
            let profile: String = profile
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            format!("{DEFAULT_KEYRING_SERVICE}-{profile}")
        }
        None if data_dir_redirected => ISOLATED_KEYRING_SERVICE.to_string(),
        None => DEFAULT_KEYRING_SERVICE.to_string(),
    }
}

/// Set by [`use_in_memory_keyring`].
static IN_MEMORY_KEYRING: AtomicBool = AtomicBool::new(false);

/// Route every keyring read, write and delete in this process to an in-memory
/// store that is gone at exit. For the tests of crates that depend on
/// mnemos-ide-sync, so they never touch a real OS login; mnemos-ide-sync's own tests always
/// run this way. Call it before the first keyring access. There is no way
/// back, because flipping to the OS store mid-test is exactly what this
/// exists to prevent.
pub fn use_in_memory_keyring() {
    IN_MEMORY_KEYRING.store(true, Ordering::SeqCst);
}

/// Whether this process keeps its admin login in the in-memory store rather
/// than the OS one, so a crate's tests can assert they never reach a real
/// login instead of trusting that the switch ran.
pub fn keyring_is_in_memory() -> bool {
    cfg!(test) || IN_MEMORY_KEYRING.load(Ordering::SeqCst)
}

/// Why the most recent keyring save in this process failed. See
/// [`AdminAuth::last_save_error`].
static LAST_SAVE_ERROR: Mutex<Option<String>> = Mutex::new(None);

/// Backing map of [`MemoryCredential`], keyed like the OS store by
/// (service, user).
static MEMORY_KEYRING: Mutex<BTreeMap<(String, String), String>> = Mutex::new(BTreeMap::new());

/// Makes the next writes to the in-memory store fail, so tests can drive the
/// "tokens refreshed but not persisted" path without a broken OS keyring.
#[cfg(test)]
static FAIL_MEMORY_WRITES: AtomicBool = AtomicBool::new(false);

/// A poisoned lock only means some other thread panicked mid-update of a
/// plain map or string; the data is still usable, and refusing it would turn
/// one failed test (or one panic in the app) into every later login failing.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// The keyring entry holding this process's admin login.
fn login_entry() -> Result<Entry, IdeError> {
    let service = keyring_service();
    // Includes `cfg!(test)` rather than relying on an opt-in: a mnemos-ide-sync test
    // that forgets to opt in must still be unable to read, overwrite or
    // delete a real login.
    if keyring_is_in_memory() {
        return Ok(Entry::new_with_credential(Box::new(MemoryCredential {
            service,
            user: KEYRING_USER.to_string(),
        })));
    }
    Ok(Entry::new(&service, KEYRING_USER)?)
}

/// In-process stand-in for the OS credential store. keyring 2.x ships a
/// `mock` builder, but it keeps each password inside the `Entry` that set
/// it, so a save followed by a load (which builds a fresh entry) would never
/// see the saved login. This one shares [`MEMORY_KEYRING`] across entries.
struct MemoryCredential {
    service: String,
    user: String,
}

impl MemoryCredential {
    fn key(&self) -> (String, String) {
        (self.service.clone(), self.user.clone())
    }
}

impl CredentialApi for MemoryCredential {
    fn set_password(&self, password: &str) -> keyring::Result<()> {
        #[cfg(test)]
        if FAIL_MEMORY_WRITES.load(Ordering::SeqCst) {
            return Err(keyring::Error::PlatformFailure(
                "injected keyring write failure".into(),
            ));
        }
        lock(&MEMORY_KEYRING).insert(self.key(), password.to_string());
        Ok(())
    }

    fn get_password(&self) -> keyring::Result<String> {
        lock(&MEMORY_KEYRING)
            .get(&self.key())
            .cloned()
            .ok_or(keyring::Error::NoEntry)
    }

    fn delete_password(&self) -> keyring::Result<()> {
        lock(&MEMORY_KEYRING)
            .remove(&self.key())
            .map(|_| ())
            .ok_or(keyring::Error::NoEntry)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Debug, Serialize)]
pub struct LoginRequest<'a> {
    pub email: &'a str,
    pub password: &'a str,
}

#[derive(Debug, Serialize)]
pub struct RefreshRequest<'a> {
    pub refresh_token: &'a str,
}

/// Wire shape for `POST /api/auth/login` and `/api/auth/refresh`. snake_case
/// matches the admin backend response verbatim.
#[derive(Debug, Deserialize)]
pub struct LoginResponse {
    pub access_token: String,
    pub refresh_token: String,
    /// Lifetime of `access_token` in seconds.
    pub expires_in: i64,
    pub user: AuthUser,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AuthUser {
    pub id: String,
    pub email: String,
}

/// Tokens received from the admin backend on login or refresh.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedAuth {
    base_url: String,
    /// Current shape (post-B3): nested tokens.
    #[serde(default)]
    tokens: Option<AdminTokens>,
    /// Legacy single-token shape (pre-B3 keyring entries). Read-only fallback;
    /// new writes always populate `tokens`.
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct AdminAuth {
    pub base_url: String,
    pub tokens: Option<AdminTokens>,
}

/// HTTP client for [`AdminAuth::login`] and [`AdminAuth::refresh`]. A bare
/// `reqwest::Client::new()` has no connect or request timeout, so a server
/// that accepts the connection and never answers would hang the call.
///
/// Built per call rather than cached: a client whose pooled connections
/// outlive the tokio runtime that opened them fails on the next runtime,
/// and every `#[tokio::test]` runs its own.
fn auth_http() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default()
}

/// Whether two admin server roots name the same server: scheme and host
/// compared case-insensitively, a trailing `/` ignored.
pub fn same_server(a: &str, b: &str) -> bool {
    fn normalize(url: &str) -> String {
        let url = url.trim().trim_end_matches('/');
        match url.split_once("://") {
            Some((scheme, rest)) => {
                let (host, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
                format!(
                    "{}://{}{path}",
                    scheme.to_ascii_lowercase(),
                    host.to_ascii_lowercase()
                )
            }
            None => url.to_string(),
        }
    }
    normalize(a) == normalize(b)
}

impl AdminAuth {
    /// A signed-out login for the admin server at `base_url`. There is no
    /// default server: every login names the server it belongs to.
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            tokens: None,
        }
    }

    /// Construct from tokens obtained outside [`Self::login`], e.g. from a
    /// `mnemos://auth/desktop_redirect?...` callback. `issuer` is the root of
    /// the server that issued them: the one the app sent the user to sign in
    /// at, never a value the redirect itself carries. Refresh and every sync
    /// request then go to that server, so tokens are never sent anywhere else.
    pub fn from_tokens(issuer: impl Into<String>, tokens: AdminTokens) -> Self {
        Self {
            base_url: issuer.into(),
            tokens: Some(tokens),
        }
    }

    /// Whether this login belongs to the admin server at `server_root`.
    pub fn is_for_server(&self, server_root: &str) -> bool {
        same_server(&self.base_url, server_root)
    }

    /// [`Self::load_from_keyring`], refusing a saved login issued by another
    /// server than `server_root` (a build pointed at a different server, or
    /// an entry written for one). Its tokens are never sent to `server_root`,
    /// and `server_root` never receives them as if they were its own.
    pub fn load_for_server(server_root: &str) -> Result<Self, IdeError> {
        let auth = Self::load_from_keyring()?;
        if !auth.is_for_server(server_root) {
            return Err(IdeError::Auth(format!(
                "the saved admin login is for {}, not {server_root}",
                auth.base_url
            )));
        }
        Ok(auth)
    }

    /// Login to the admin platform. Hits `POST {base_url}/api/auth/login` and
    /// populates `tokens` on success.
    ///
    /// Returns `Ok` even when persisting the new tokens fails; see
    /// [`Self::last_save_error`].
    pub async fn login(&mut self, email: &str, password: &str) -> Result<(), IdeError> {
        let url = format!("{}/api/auth/login", self.base_url);
        let body = LoginRequest { email, password };
        let resp = auth_http().post(&url).json(&body).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(IdeError::Auth(format!("login failed: {status} {text}")));
        }
        let parsed: LoginResponse = resp.json().await?;
        self.store_tokens(parsed);
        Ok(())
    }

    /// Refresh the access token using the stored refresh token.
    /// Updates `self.tokens` in-place on success.
    ///
    /// Returns `Ok` even when persisting the rotated tokens fails; see
    /// [`Self::last_save_error`].
    pub async fn refresh(&mut self) -> Result<(), IdeError> {
        self.refresh_with(&auth_http()).await
    }

    /// [`Self::refresh`] on the caller's HTTP client, so the refresh is
    /// bounded by that client's timeouts. A caller that holds a lock across
    /// the refresh needs that bound: without it, a server that accepts the
    /// connection and never answers holds the lock indefinitely.
    pub async fn refresh_with(&mut self, http: &reqwest::Client) -> Result<(), IdeError> {
        let refresh_token = self
            .tokens
            .as_ref()
            .map(|t| t.refresh_token.clone())
            .ok_or(IdeError::NotAuthenticated)?;

        let url = format!("{}/api/auth/refresh", self.base_url);
        let body = RefreshRequest {
            refresh_token: &refresh_token,
        };
        let resp = http.post(&url).json(&body).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(IdeError::Auth(format!("refresh failed: {status} {text}")));
        }
        let parsed: LoginResponse = resp.json().await?;
        self.store_tokens(parsed);
        Ok(())
    }

    /// Adopt the tokens from a login or refresh response and persist them.
    ///
    /// A failed save does not fail the call: the new tokens are good for this
    /// session, and throwing them away would log the user out now instead of
    /// on the next launch. The failure is kept for [`Self::last_save_error`].
    fn store_tokens(&mut self, parsed: LoginResponse) {
        self.tokens = Some(AdminTokens {
            access_token: parsed.access_token,
            refresh_token: parsed.refresh_token,
            expires_at: Utc::now() + Duration::seconds(parsed.expires_in),
        });
        // Persist rotated tokens — otherwise the next app start loads the
        // stale pair from the keyring and has to refresh again (or fails if
        // the server rotates refresh tokens one-time).
        if let Err(e) = self.save_to_keyring() {
            tracing::warn!(target: "mnemos_ide_sync::auth", "keyring save after token update failed: {e}");
        }
    }

    pub fn is_authenticated(&self) -> bool {
        match &self.tokens {
            Some(t) => Utc::now() < t.expires_at,
            None => false,
        }
    }

    /// Bearer access token for use in Authorization headers.
    pub fn bearer_token(&self) -> Result<&str, IdeError> {
        if !self.is_authenticated() {
            return Err(IdeError::NotAuthenticated);
        }
        Ok(self.tokens.as_ref().unwrap().access_token.as_str())
    }

    /// Why the most recent keyring save in this process failed, or `None`
    /// when it succeeded, a later [`Self::clear_keyring`] succeeded, or no
    /// save has been attempted.
    ///
    /// [`Self::login`] and [`Self::refresh`] return `Ok` when only the save
    /// fails, so this is how a caller learns that the login in the keyring is
    /// stale. If the server rotated the refresh token, that stale pair no
    /// longer works and the next launch starts logged out.
    ///
    /// Process-wide rather than per instance: every `AdminAuth` in the
    /// process writes the same keyring entry ([`keyring_service`]), so the
    /// outcome describes that entry, whichever clone did the writing.
    pub fn last_save_error() -> Option<String> {
        lock(&LAST_SAVE_ERROR).clone()
    }

    pub fn save_to_keyring(&self) -> Result<(), IdeError> {
        let result = self.write_keyring();
        *lock(&LAST_SAVE_ERROR) = result.as_ref().err().map(|e| e.to_string());
        result
    }

    fn write_keyring(&self) -> Result<(), IdeError> {
        let payload = PersistedAuth {
            base_url: self.base_url.clone(),
            tokens: self.tokens.clone(),
            token: None,
            expires_at: None,
        };
        let json = serde_json::to_string(&payload)?;
        login_entry()?.set_password(&json)?;
        Ok(())
    }

    pub fn load_from_keyring() -> Result<Self, IdeError> {
        let entry = login_entry()?;
        let json = match entry.get_password() {
            Ok(j) => j,
            Err(keyring::Error::NoEntry) => return Err(IdeError::NotAuthenticated),
            Err(e) => return Err(e.into()),
        };
        let payload: PersistedAuth = serde_json::from_str(&json)?;
        let tokens = payload.tokens.or_else(|| {
            // Legacy shape: pre-B3 keyring entries had a single `token` field
            // and no separate access/refresh. Treat the legacy token as both
            // until refresh — a refresh call will replace it with real shape.
            match (payload.token, payload.expires_at) {
                (Some(t), Some(exp)) => Some(AdminTokens {
                    access_token: t.clone(),
                    refresh_token: t,
                    expires_at: exp,
                }),
                _ => None,
            }
        });
        Ok(Self {
            base_url: payload.base_url,
            tokens,
        })
    }

    pub fn clear_keyring() -> Result<(), IdeError> {
        let entry = login_entry()?;
        match entry.delete_password() {
            Ok(()) | Err(keyring::Error::NoEntry) => {
                // Nothing is left in the keyring to be stale, so an earlier
                // failed save no longer means anything.
                *lock(&LAST_SAVE_ERROR) = None;
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes the tests that write the keyring entry: they share one
    /// entry and the process-wide `LAST_SAVE_ERROR`.
    static KEYRING_TESTS: Mutex<()> = Mutex::new(());

    /// Redirect the Mnemos IDE data dir, as the manager tests do, before any keyring
    /// access. `keyring_service()` changes when the redirect lands, and
    /// manager tests running in parallel land it at an arbitrary moment; doing
    /// it first keeps a save and the load after it on the same entry.
    fn isolate_data_dir() {
        static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
        let dir = DIR.get_or_init(|| tempfile::tempdir().unwrap());
        let _ = mnemos_ide_core::paths::override_data_dir(dir.path().to_path_buf());
    }

    /// Clears `FAIL_MEMORY_WRITES` even when an assertion panics first.
    struct FailWrites;

    impl FailWrites {
        fn start() -> Self {
            FAIL_MEMORY_WRITES.store(true, Ordering::SeqCst);
            Self
        }
    }

    impl Drop for FailWrites {
        fn drop(&mut self) {
            FAIL_MEMORY_WRITES.store(false, Ordering::SeqCst);
        }
    }

    const SERVER: &str = "https://admin.example.test";

    fn tokens(access: &str) -> AdminTokens {
        AdminTokens {
            access_token: access.to_string(),
            refresh_token: "RT".to_string(),
            expires_at: Utc::now() + Duration::hours(1),
        }
    }

    #[test]
    fn signed_out_login_is_unauthenticated() {
        let a = AdminAuth::with_base_url(SERVER);
        assert!(!a.is_authenticated());
        assert!(matches!(a.bearer_token(), Err(IdeError::NotAuthenticated)));
    }

    #[test]
    fn from_tokens_constructs_authenticated() {
        let a = AdminAuth::from_tokens(SERVER, AdminTokens {
            access_token: "AT".to_string(),
            refresh_token: "RT".to_string(),
            expires_at: Utc::now() + Duration::hours(1),
        });
        assert!(a.is_authenticated());
        assert_eq!(a.bearer_token().unwrap(), "AT");
    }

    /// Regression: `from_tokens` used to stamp a hard-coded TwinCurrent URL
    /// on every login, whichever server issued it, so a build pointed at
    /// another server sent that server's refresh token to TwinCurrent.
    #[test]
    fn from_tokens_keeps_the_issuing_server() {
        let a = AdminAuth::from_tokens("https://sync.example.org", tokens("AT"));
        assert_eq!(a.base_url, "https://sync.example.org");
        assert!(a.is_for_server("https://sync.example.org/"));
        assert!(a.is_for_server("HTTPS://Sync.Example.org"));
        assert!(!a.is_for_server("https://admin.twincurrent.dk"));
        assert!(!a.is_for_server("https://sync.example.org.evil.test"));
        assert!(!a.is_for_server("http://sync.example.org"));
    }

    #[test]
    fn saved_login_for_another_server_is_refused() {
        isolate_data_dir();
        let _serial = lock(&KEYRING_TESTS);
        AdminAuth::from_tokens("https://sync.example.org", tokens("AT"))
            .save_to_keyring()
            .expect("save");
        let loaded = AdminAuth::load_for_server("https://sync.example.org/").expect("same server");
        assert_eq!(loaded.tokens.unwrap().access_token, "AT");
        match AdminAuth::load_for_server("https://admin.twincurrent.dk") {
            Err(IdeError::Auth(msg)) => assert!(msg.contains("sync.example.org"), "{msg}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
        AdminAuth::clear_keyring().expect("clear");
    }

    #[test]
    fn expired_tokens_unauthenticated() {
        let a = AdminAuth::from_tokens(SERVER, AdminTokens {
            access_token: "AT".to_string(),
            refresh_token: "RT".to_string(),
            expires_at: Utc::now() - Duration::hours(1),
        });
        assert!(!a.is_authenticated());
        assert!(matches!(a.bearer_token(), Err(IdeError::NotAuthenticated)));
    }

    #[test]
    fn keyring_service_naming_rules() {
        // Default install: the pre-isolation name, so upgrades keep the login.
        assert_eq!(keyring_service_for(None, false), "tcbv2");
        // Redirected data dir, no profile.
        assert_eq!(keyring_service_for(None, true), "tcbv2-isolated");
        // A profile names the entry whether or not the dir is redirected.
        assert_eq!(keyring_service_for(Some("dev"), false), "tcbv2-dev");
        assert_eq!(keyring_service_for(Some("dev"), true), "tcbv2-dev");
        // An empty profile counts as unset.
        assert_eq!(keyring_service_for(Some(""), false), "tcbv2");
        assert_eq!(keyring_service_for(Some(""), true), "tcbv2-isolated");
    }

    #[test]
    fn keyring_service_sanitizes_the_profile() {
        assert_eq!(keyring_service_for(Some("Dev_2-b"), false), "tcbv2-Dev_2-b");
        assert_eq!(
            keyring_service_for(Some("my profile/ø.1"), false),
            "tcbv2-my_profile___1"
        );
        assert_eq!(keyring_service_for(Some("a.b"), false), "tcbv2-a_b");
    }

    #[test]
    fn isolated_processes_never_get_the_default_entry() {
        for profile in ["x", " ", ".", "tcbv2", "\u{0}", "../tcbv2"] {
            for redirected in [false, true] {
                assert_ne!(
                    keyring_service_for(Some(profile), redirected),
                    DEFAULT_KEYRING_SERVICE,
                    "profile {profile:?}, redirected {redirected}"
                );
            }
        }
        assert_ne!(keyring_service_for(None, true), DEFAULT_KEYRING_SERVICE);
    }

    #[test]
    fn redirected_process_uses_an_isolated_service() {
        isolate_data_dir();
        let service = keyring_service();
        assert_eq!(
            service,
            keyring_service_for(mnemos_ide_core::paths::data_profile().as_deref(), true)
        );
        assert_ne!(service, DEFAULT_KEYRING_SERVICE);
    }

    #[test]
    fn tests_never_reach_the_os_keyring() {
        let entry = login_entry().expect("entry");
        assert!(
            entry
                .get_credential()
                .downcast_ref::<MemoryCredential>()
                .is_some(),
            "mnemos-ide-sync tests must run against the in-memory keyring"
        );
    }

    #[test]
    fn keyring_round_trip() {
        isolate_data_dir();
        let _serial = lock(&KEYRING_TESTS);
        let a = AdminAuth::from_tokens(SERVER, tokens("AT"));
        a.save_to_keyring().expect("save");
        let loaded = AdminAuth::load_from_keyring().expect("load");
        assert_eq!(loaded.tokens.as_ref().unwrap().access_token, "AT");
        assert_eq!(loaded.tokens.as_ref().unwrap().refresh_token, "RT");
        assert_eq!(loaded.base_url, a.base_url);

        AdminAuth::clear_keyring().expect("clear");
        assert!(matches!(
            AdminAuth::load_from_keyring(),
            Err(IdeError::NotAuthenticated)
        ));
        // Clearing twice is fine: logout must not fail on an empty keyring.
        AdminAuth::clear_keyring().expect("clear again");
    }

    #[test]
    fn failed_save_is_reported_until_a_save_succeeds() {
        isolate_data_dir();
        let _serial = lock(&KEYRING_TESTS);
        let a = AdminAuth::from_tokens(SERVER, tokens("AT"));
        {
            let _fail = FailWrites::start();
            assert!(a.save_to_keyring().is_err());
        }
        let err = AdminAuth::last_save_error().expect("save error recorded");
        assert!(err.contains("injected keyring write failure"), "{err}");

        a.save_to_keyring().expect("save");
        assert_eq!(AdminAuth::last_save_error(), None);
        AdminAuth::clear_keyring().expect("clear");
    }

    #[test]
    fn token_update_survives_a_failed_save() {
        isolate_data_dir();
        let _serial = lock(&KEYRING_TESTS);
        AdminAuth::from_tokens(SERVER, tokens("OLD"))
            .save_to_keyring()
            .expect("seed");

        let mut a = AdminAuth::from_tokens(SERVER, tokens("OLD"));
        {
            let _fail = FailWrites::start();
            // The step `login()` and `refresh()` take after a 2xx response.
            a.store_tokens(LoginResponse {
                access_token: "NEW".to_string(),
                refresh_token: "RT2".to_string(),
                expires_in: 3600,
                user: AuthUser {
                    id: "u".to_string(),
                    email: "u@example.com".to_string(),
                },
            });
        }
        // This session keeps working on the new tokens...
        assert_eq!(a.bearer_token().unwrap(), "NEW");
        // ...the caller can tell the keyring still holds the old pair...
        assert!(AdminAuth::last_save_error().is_some());
        let stored = AdminAuth::load_from_keyring().expect("load");
        assert_eq!(stored.tokens.unwrap().access_token, "OLD");
        // ...and a logout leaves nothing stale to report.
        AdminAuth::clear_keyring().expect("clear");
        assert_eq!(AdminAuth::last_save_error(), None);
    }
}
