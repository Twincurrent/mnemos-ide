// SPDX-License-Identifier: AGPL-3.0-only
//! The [`SyncClient`] side of end-to-end encryption ([`crate::crypto`]).
//!
//! The state and blob calls the manager and the transcript sync use
//! (`get_state`, `put_state`, `delete_state`, `pull_all`, `put_blob`,
//! `get_blob`, `list_blobs`, `delete_blob`) take logical names. When the
//! account is encrypted they are routed to the encrypted namespace and
//! sealed or opened on the way, so the rest of the crate never sees
//! ciphertext or the `e2e:` names. Versions are the encrypted key's, which
//! is consistent because every read and write of a logical key goes to the
//! same namespace.
//!
//! Whether the account is encrypted is learned lazily: from the marker in
//! the first full pull, or with one GET of the marker before the first
//! personal call that comes earlier. Until a machine has the key, personal
//! calls fail (nothing local is touched) and the live feed keeps working.
//!
//! A machine that has a key saved for the account but finds no marker does
//! not fall back to plaintext: the status is `MarkerMissing`, which locks
//! personal calls like a missing key does, until the marker is put back.
//!
//! Plaintext under a personal name while encryption is on (from a build
//! from before it, or from before the switch) is absorbed only during the
//! migration window sealed in the marker ([`SyncKey::migration_until`]): a
//! full pull merges it into the encrypted copy, then deletes the plaintext
//! if nobody rewrote it meanwhile; a transcript listing re-uploads such
//! blobs encrypted and deletes the plaintext. That is how existing data is
//! migrated when encryption is turned on. After the window plaintext is
//! ignored (counted in [`E2eReport::ignored`]): taking it would let the
//! server write anything into `~/.claude` by not sending an envelope.
//!
//! The absorb deletes the plaintext with a conditional delete
//! (`?expected_version=`), so a write by an older build after the value was
//! read is refused by the server and absorbed by the next pull. A server
//! from before the conditional delete ignores the version; the version check
//! before the delete and the window bound the exposure there. Blobs have no
//! versions: a plaintext segment rewritten between its re-upload and its
//! delete is lost, but segments are written once and never rewritten.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::time::SystemTime;

use serde_json::Value;

use crate::client::{
    validate_blob_path, validate_blob_prefix, BlobItem, SyncClient, SyncStateResponse,
};
use crate::crypto::{
    is_encryptable_blob, is_encryptable_key, is_rollback, resolve_status, unix_ms, E2eError,
    E2eStatus, SyncKey, E2E_BLOB_PREFIX, E2E_KEY_PREFIX, E2E_META_KEY, TRANSCRIPT_BLOB_PREFIX,
};
use crate::error::IdeError;
use crate::merge::{merge_for_key, PLATFORM_KEYS};

const TARGET: &str = "mnemos_ide_sync::e2e";

/// Start of the error a personal call returns while the account is
/// encrypted and this machine cannot read it.
pub const LOCKED_PREFIX: &str = "end-to-end encryption: ";

/// What this client knows about the account's encryption.
#[derive(Debug, Default)]
pub struct E2eState {
    /// The key saved on this machine for the signed-in account, if any.
    pub(crate) local_key: Option<Arc<SyncKey>>,
    pub(crate) status: E2eStatus,
    /// Plaintext values and blobs absorbed into the encrypted namespace.
    pub(crate) absorbed: u64,
    /// When plaintext last turned up while encryption was on.
    pub(crate) last_absorbed_at: Option<SystemTime>,
    /// Logical keys whose encrypted value could not be opened in the last
    /// full pull (damaged, sealed with another key, or an older copy).
    pub(crate) unreadable: Vec<String>,
    /// End of the marker's migration window (unix seconds), when the marker
    /// has one sealed with the local key.
    pub(crate) migrate_until: Option<i64>,
    /// Highest `seq` opened or sealed per logical key this run (rollback).
    pub(crate) seen_seq: HashMap<String, u64>,
    /// Personal keys and blob paths found in plaintext after the migration
    /// window, and so ignored.
    pub(crate) ignored: BTreeSet<String>,
}

impl E2eState {
    /// Whether plaintext personal values are still absorbed, at `now`.
    pub(crate) fn migrating_at(&self, now: SystemTime) -> bool {
        let now_secs = i64::try_from(unix_ms(now) / 1000).unwrap_or(i64::MAX);
        matches!(self.status, E2eStatus::On { .. })
            && self.migrate_until.is_some_and(|until| now_secs < until)
    }
}

/// What [`SyncClient::e2e_report`] shows the settings page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct E2eReport {
    pub status: E2eStatus,
    pub absorbed: u64,
    pub last_absorbed_at: Option<SystemTime>,
    pub unreadable: Vec<String>,
    /// Plaintext personal keys and blobs ignored because the migration
    /// window is over (an older build is probably still syncing).
    pub ignored: Vec<String>,
    /// Whether plaintext is still being migrated in.
    pub migrating: bool,
}

/// Where a call goes.
enum Route {
    Plain,
    Sealed(Arc<SyncKey>),
}

impl SyncClient {
    fn e2e_read(&self) -> std::sync::RwLockReadGuard<'_, E2eState> {
        self.e2e.read().unwrap_or_else(|e| e.into_inner())
    }

    fn e2e_write(&self) -> std::sync::RwLockWriteGuard<'_, E2eState> {
        self.e2e.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Where this machine stands with the account's encryption.
    pub fn e2e_status(&self) -> E2eStatus {
        self.e2e_read().status.clone()
    }

    /// Status plus what the absorption of plaintext has done this run.
    pub fn e2e_report(&self) -> E2eReport {
        let st = self.e2e_read();
        E2eReport {
            status: st.status.clone(),
            absorbed: st.absorbed,
            last_absorbed_at: st.last_absorbed_at,
            unreadable: st.unreadable.clone(),
            ignored: st.ignored.iter().cloned().collect(),
            migrating: st.migrating_at(SystemTime::now()),
        }
    }

    /// The key saved on this machine for the signed-in account. The status
    /// is worked out again on the next call.
    pub fn set_local_e2e_key(&self, key: Option<SyncKey>) {
        let mut st = self.e2e_write();
        st.local_key = key.map(Arc::new);
        st.status = E2eStatus::Unknown;
        st.migrate_until = None;
    }

    /// Learn the account's status from its marker, once per run (and again
    /// after [`Self::set_local_e2e_key`]).
    pub async fn resolve_e2e(&self) -> Result<E2eStatus, IdeError> {
        let status = self.e2e_status();
        if status != E2eStatus::Unknown {
            return Ok(status);
        }
        let meta = self.get_state_raw(E2E_META_KEY).await?;
        Ok(self.note_meta(meta.as_ref().map(|m| &m.value)))
    }

    /// Fold what a pull or a GET found under the marker into the status.
    fn note_meta(&self, meta: Option<&Value>) -> E2eStatus {
        let mut st = self.e2e_write();
        let current = st.status.clone();
        let local_key = st.local_key.clone();
        let next = match (&current, meta) {
            // A marker that vanished while this machine encrypts ("Delete
            // synced data" from another machine, or the server lost it):
            // pause, as a fresh start with the key saved here would. Never
            // plaintext, and no longer encrypting either: uploads under
            // `e2e:` with no marker would split the account between machines
            // that start over unencrypted and this one, which would absorb
            // and delete their plaintext. A pull that finds the marker again
            // resumes; Settings > Sync can restore it or start over.
            (E2eStatus::On { .. }, None) => {
                tracing::warn!(target: TARGET, "the account's encryption marker is gone; personal sync paused");
                E2eStatus::MarkerMissing
            }
            _ => {
                let next = resolve_status(meta, local_key.as_deref());
                st.migrate_until = match (&next, meta, local_key.as_deref()) {
                    (E2eStatus::On { .. }, Some(meta), Some(key)) => key.migration_until(meta),
                    _ => None,
                };
                next
            }
        };
        if next != st.status {
            if next == E2eStatus::MarkerMissing {
                tracing::warn!(target: TARGET, "a recovery key is saved here but the account has no encryption marker; personal sync paused");
            }
            tracing::info!(target: TARGET, status = ?next, "end-to-end encryption status");
        }
        st.status = next.clone();
        next
    }

    /// Whether plaintext personal values are absorbed right now.
    fn migrating(&self) -> bool {
        self.e2e_read().migrating_at(SystemTime::now())
    }

    fn note_ignored(&self, name: &str) {
        let mut st = self.e2e_write();
        if st.ignored.insert(name.to_string()) {
            tracing::warn!(target: TARGET, item = %name, "plaintext personal data while encryption is on, after the migration window; ignored");
        }
    }

    /// Seal `value` for `key` with a `seq` past every one seen for it here.
    /// The `seq` is recorded with [`Self::note_seq`] once the server took
    /// the write.
    fn seal_next(&self, sk: &SyncKey, key: &str, value: &Value) -> (Value, u64) {
        let floor = self
            .e2e_read()
            .seen_seq
            .get(key)
            .map_or(0, |s| s.saturating_add(1));
        let seq = unix_ms(SystemTime::now()).max(floor);
        (sk.seal_state_seq(key, value, seq), seq)
    }

    fn note_seq(&self, key: &str, seq: u64) {
        let mut st = self.e2e_write();
        let seen = st.seen_seq.entry(key.to_string()).or_insert(seq);
        *seen = (*seen).max(seq);
    }

    /// Open a state value for `logical`, refusing an older copy than one
    /// already seen here (see [`crate::crypto`] on rollback).
    fn open_value(&self, sk: &SyncKey, logical: &str, envelope: &Value) -> Result<Value, E2eError> {
        let (value, seq) = sk.open_state_seq(logical, envelope)?;
        let mut st = self.e2e_write();
        let seen = st.seen_seq.get(logical).copied();
        if is_rollback(seen, seq) {
            return Err(E2eError::Rollback);
        }
        if seen.is_none_or(|seen| seq > seen) {
            st.seen_seq.insert(logical.to_string(), seq);
        }
        Ok(value)
    }

    fn open_response(
        &self,
        sk: &SyncKey,
        logical: &str,
        mut resp: SyncStateResponse,
    ) -> Result<SyncStateResponse, IdeError> {
        resp.value = self
            .open_value(sk, logical, &resp.value)
            .map_err(|e| IdeError::Other(format!("{LOCKED_PREFIX}{logical}: {e}")))?;
        resp.key = logical.to_string();
        Ok(resp)
    }

    async fn route_key(&self, key: &str) -> Result<Route, IdeError> {
        if !is_encryptable_key(key) {
            return Ok(Route::Plain);
        }
        self.route_personal().await
    }

    async fn route_blob(&self, path: &str) -> Result<Route, IdeError> {
        if !is_encryptable_blob(path) {
            return Ok(Route::Plain);
        }
        self.route_personal().await
    }

    async fn route_personal(&self) -> Result<Route, IdeError> {
        match self.resolve_e2e().await? {
            E2eStatus::Off => Ok(Route::Plain),
            E2eStatus::On { .. } => match self.e2e_read().local_key.clone() {
                Some(key) => Ok(Route::Sealed(key)),
                None => Err(locked_error(&E2eStatus::NeedsKey)),
            },
            locked => Err(locked_error(&locked)),
        }
    }

    // ── State ───────────────────────────────────────────────────────────────

    /// `GET /api/tcb/state/:key` for a logical key. `Ok(None)` when absent.
    pub async fn get_state(&self, key: &str) -> Result<Option<SyncStateResponse>, IdeError> {
        match self.route_key(key).await? {
            Route::Plain => self.get_state_raw(key).await,
            Route::Sealed(sk) => {
                let Some(resp) = self.get_state_raw(&sealed_key(key)).await? else {
                    return Ok(None);
                };
                Ok(Some(self.open_response(&sk, key, resp)?))
            }
        }
    }

    /// `POST /api/tcb/state` for a logical key, with optional CAS. The
    /// response and a conflict carry the logical key and plaintext.
    pub async fn put_state(
        &self,
        key: &str,
        value: &Value,
        expected_version: Option<i64>,
    ) -> Result<SyncStateResponse, IdeError> {
        match self.route_key(key).await? {
            Route::Plain => self.put_state_raw(key, value, expected_version).await,
            Route::Sealed(sk) => {
                let (sealed, seq) = self.seal_next(&sk, key, value);
                match self
                    .put_state_raw(&sealed_key(key), &sealed, expected_version)
                    .await
                {
                    Ok(mut resp) => {
                        self.note_seq(key, seq);
                        resp.key = key.to_string();
                        resp.value = value.clone();
                        Ok(resp)
                    }
                    Err(IdeError::CasConflict {
                        current_version,
                        current_value,
                        ..
                    }) => Err(IdeError::CasConflict {
                        key: key.to_string(),
                        current_version,
                        current_value: if current_value.is_null() {
                            Value::Null
                        } else {
                            self.open_value(&sk, key, &current_value)
                                .unwrap_or(Value::Null)
                        },
                    }),
                    Err(e) => Err(e),
                }
            }
        }
    }

    /// `DELETE /api/tcb/state/:key` for a logical key.
    pub async fn delete_state(&self, key: &str) -> Result<(), IdeError> {
        match self.route_key(key).await? {
            Route::Plain => self.delete_state_raw(key).await,
            Route::Sealed(_) => self.delete_state_raw(&sealed_key(key)).await,
        }
    }

    /// `GET /api/tcb/state`: every state value under its logical key.
    /// Encrypted values come back opened, plaintext personal values written
    /// while encryption is on are absorbed first, and the marker and the
    /// encrypted namespace never show. While the account is locked to this
    /// machine only the non-personal keys come back.
    pub async fn pull_all(&self) -> Result<Vec<SyncStateResponse>, IdeError> {
        let states = self.pull_all_raw().await?;
        let meta = states
            .iter()
            .find(|s| s.key == E2E_META_KEY)
            .map(|s| &s.value);
        let status = self.note_meta(meta);
        let key = match &status {
            E2eStatus::On { .. } => self.e2e_read().local_key.clone(),
            _ => None,
        };
        let Some(sk) = key else {
            let locked = status.is_locked();
            return Ok(states
                .into_iter()
                .filter(|s| !s.key.starts_with(E2E_KEY_PREFIX) && s.key != E2E_META_KEY)
                .filter(|s| !locked || !is_encryptable_key(&s.key))
                .collect());
        };

        let mut passthrough = Vec::new();
        let mut opened: BTreeMap<String, SyncStateResponse> = BTreeMap::new();
        let mut legacy = Vec::new();
        let mut unreadable = Vec::new();
        for s in states {
            if s.key == E2E_META_KEY {
                continue;
            }
            if let Some(logical) = s.key.strip_prefix(E2E_KEY_PREFIX) {
                let logical = logical.to_string();
                match self.open_response(&sk, &logical, s) {
                    Ok(resp) => {
                        opened.insert(logical, resp);
                    }
                    Err(e) => {
                        tracing::warn!(target: TARGET, key = %logical, "encrypted value not readable: {e}");
                        unreadable.push(logical);
                    }
                }
            } else if is_encryptable_key(&s.key) {
                legacy.push(s);
            } else {
                passthrough.push(s);
            }
        }
        let migrating = self.migrating();
        for plain in legacy {
            // Outside the migration window plaintext is never taken: the
            // server could otherwise write anything here by not sealing it.
            if !migrating {
                self.note_ignored(&plain.key);
                continue;
            }
            // A value this machine cannot open is never merged over: the
            // plaintext stays until the encrypted copy is readable again.
            if unreadable.contains(&plain.key) {
                continue;
            }
            let existing = opened.get(&plain.key).cloned();
            match self.absorb(&sk, &plain, existing.as_ref()).await {
                Ok(Some(resp)) => {
                    opened.insert(plain.key.clone(), resp);
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(target: TARGET, key = %plain.key, "plaintext value not absorbed yet: {e:#}");
                }
            }
        }
        self.e2e_write().unreadable = unreadable;
        passthrough.extend(opened.into_values());
        Ok(passthrough)
    }

    /// Merge one plaintext personal value into its encrypted copy and delete
    /// the plaintext. Returns the encrypted copy as it now stands (opened),
    /// or `None` when it was left for the next pull (a racing write).
    async fn absorb(
        &self,
        sk: &SyncKey,
        plain: &SyncStateResponse,
        existing: Option<&SyncStateResponse>,
    ) -> Result<Option<SyncStateResponse>, IdeError> {
        let target = sealed_key(&plain.key);
        let written = match existing {
            None => {
                let (sealed, seq) = self.seal_next(sk, &plain.key, &plain.value);
                match self.put_state_raw(&target, &sealed, Some(0)).await {
                    Ok(mut resp) => {
                        self.note_seq(&plain.key, seq);
                        resp.key = plain.key.clone();
                        resp.value = plain.value.clone();
                        resp
                    }
                    Err(IdeError::CasConflict { .. }) => return Ok(None),
                    Err(e) => return Err(e),
                }
            }
            Some(enc) => {
                let plain_newer = newer(&plain.synced_at, &enc.synced_at);
                let merged = merge_absorbed(&plain.key, &plain.value, &enc.value, plain_newer);
                if merged == enc.value {
                    enc.clone()
                } else {
                    let (sealed, seq) = self.seal_next(sk, &plain.key, &merged);
                    match self
                        .put_state_raw(&target, &sealed, Some(enc.version))
                        .await
                    {
                        Ok(mut resp) => {
                            self.note_seq(&plain.key, seq);
                            resp.key = plain.key.clone();
                            resp.value = merged;
                            resp
                        }
                        Err(IdeError::CasConflict { .. }) => return Ok(None),
                        Err(e) => return Err(e),
                    }
                }
            }
        };
        // Deleted only when nobody rewrote it since it was read: a write
        // racing this one is absorbed by the next pull instead of lost. The
        // delete itself carries the version, so the server refuses it (409)
        // when an older build wrote between the check and the delete; the
        // check before it keeps a server without the conditional delete
        // (which ignores the version) to the narrow window it had. The key
        // goes into the URL with `/` escaped: memory keys
        // (`claude-memory:<profile>:<owner>/<repo>`) hold one, and the
        // server's `/state/{key}` route matches a single path segment.
        let exact = path_key(&plain.key);
        let removed = match self.get_state_raw(&exact).await? {
            Some(now) if now.version == plain.version => {
                self.delete_state_raw_at(&exact, plain.version).await?
            }
            _ => false,
        };
        if removed {
            let mut st = self.e2e_write();
            st.absorbed += 1;
            st.last_absorbed_at = Some(SystemTime::now());
        }
        tracing::info!(
            target: TARGET,
            key = %plain.key,
            removed,
            "plaintext value encrypted into the e2e namespace",
        );
        Ok(Some(written))
    }

    // ── Blobs ───────────────────────────────────────────────────────────────

    /// `PUT /api/tcb/blobs/{path}` for a logical path.
    pub async fn put_blob(&self, path: &str, bytes: Vec<u8>) -> Result<(), IdeError> {
        validate_blob_path(path)?;
        match self.route_blob(path).await? {
            Route::Plain => self.put_blob_raw(path, bytes).await,
            Route::Sealed(sk) => {
                let sealed = sk.seal_blob(path, &bytes);
                self.put_blob_raw(&sealed_blob(path), sealed).await
            }
        }
    }

    /// `GET /api/tcb/blobs/{path}` for a logical path. While encrypted, a
    /// blob not absorbed yet is read from its plaintext copy, during the
    /// migration window only.
    pub async fn get_blob(&self, path: &str) -> Result<Option<Vec<u8>>, IdeError> {
        validate_blob_path(path)?;
        match self.route_blob(path).await? {
            Route::Plain => self.get_blob_raw(path).await,
            Route::Sealed(sk) => match self.get_blob_raw(&sealed_blob(path)).await? {
                Some(bytes) => sk
                    .open_blob(path, &bytes)
                    .map(Some)
                    .map_err(|e| IdeError::Other(format!("{LOCKED_PREFIX}blob {path}: {e}"))),
                None if self.migrating() => self.get_blob_raw(path).await,
                None => Ok(None),
            },
        }
    }

    /// `GET /api/tcb/blobs?prefix=` under logical paths. While encrypted, a
    /// transcript listing also re-uploads every plaintext blob it finds
    /// encrypted and deletes the plaintext (keeping its upload time in the
    /// result, which orders segments), during the migration window only;
    /// after it plaintext blobs are left out.
    pub async fn list_blobs(&self, prefix: &str) -> Result<Vec<BlobItem>, IdeError> {
        validate_blob_prefix(prefix)?;
        if !prefix.starts_with(TRANSCRIPT_BLOB_PREFIX) {
            let items = self.list_blobs_raw(prefix).await?;
            return Ok(items
                .into_iter()
                .filter(|i| !i.path.starts_with(E2E_BLOB_PREFIX))
                .collect());
        }
        let sk = match self.route_personal().await? {
            Route::Plain => return self.list_blobs_raw(prefix).await,
            Route::Sealed(sk) => sk,
        };
        let mut out: BTreeMap<String, BlobItem> = BTreeMap::new();
        for mut item in self.list_blobs_raw(&sealed_blob(prefix)).await? {
            if let Some(logical) = item.path.strip_prefix(E2E_BLOB_PREFIX) {
                item.path = logical.to_string();
                out.insert(item.path.clone(), item);
            }
        }
        let migrating = self.migrating();
        for item in self.list_blobs_raw(prefix).await? {
            if !migrating {
                self.note_ignored(&item.path);
                continue;
            }
            match self.absorb_blob(&sk, &item.path).await {
                Ok(()) => {}
                Err(e) => {
                    // Still listed: `get_blob` falls back to the plaintext.
                    tracing::warn!(target: TARGET, path = %item.path, "plaintext blob not absorbed yet: {e:#}");
                }
            }
            out.insert(item.path.clone(), item);
        }
        Ok(out.into_values().collect())
    }

    async fn absorb_blob(&self, sk: &SyncKey, path: &str) -> Result<(), IdeError> {
        let Some(bytes) = self.get_blob_raw(path).await? else {
            return Ok(());
        };
        self.put_blob_raw(&sealed_blob(path), sk.seal_blob(path, &bytes))
            .await?;
        self.delete_blob_raw(path).await?;
        let mut st = self.e2e_write();
        st.absorbed += 1;
        st.last_absorbed_at = Some(SystemTime::now());
        Ok(())
    }

    /// `DELETE /api/tcb/blobs/{path}` for a logical path (both copies while
    /// encrypted).
    pub async fn delete_blob(&self, path: &str) -> Result<(), IdeError> {
        validate_blob_path(path)?;
        match self.route_blob(path).await? {
            Route::Plain => self.delete_blob_raw(path).await,
            Route::Sealed(_) => {
                self.delete_blob_raw(&sealed_blob(path)).await?;
                self.delete_blob_raw(path).await
            }
        }
    }

    // ── Turning it on, entering the key ─────────────────────────────────────

    /// Turn encryption on for the account with `key`, which the caller has
    /// already saved on this machine. Fails when it is already on (another
    /// machine got there first: enter that machine's recovery key instead).
    /// Existing data is encrypted by the next full pull and transcript
    /// listing, which the caller starts by restarting sync.
    ///
    /// With the status `MarkerMissing` and `key` the one saved here, this
    /// puts the marker back instead, without a migration window: nothing
    /// unencrypted on the server is taken in.
    pub async fn enable_e2e(&self, key: &SyncKey) -> Result<(), IdeError> {
        let restoring = {
            let st = self.e2e_read();
            st.status == E2eStatus::MarkerMissing && st.local_key.as_deref() == Some(key)
        };
        if self.get_state_raw(E2E_META_KEY).await?.is_some() {
            return Err(IdeError::Other(format!(
                "{LOCKED_PREFIX}already on for this account; enter its recovery key instead"
            )));
        }
        let now = mnemos_ide_core::registry::format_timestamp(chrono::Utc::now());
        let meta = if restoring {
            key.new_meta_without_migration(&now)
        } else {
            key.new_meta(&now)
        };
        match self.put_state_raw(E2E_META_KEY, &meta, Some(0)).await {
            Ok(_) => {}
            Err(IdeError::CasConflict { .. }) => {
                return Err(IdeError::Other(format!(
                    "{LOCKED_PREFIX}another machine turned it on a moment ago; enter its recovery key instead"
                )));
            }
            Err(e) => return Err(e),
        }
        let mut st = self.e2e_write();
        st.local_key = Some(Arc::new(key.clone()));
        st.status = E2eStatus::On {
            key_id: key.key_id_hex(),
        };
        st.migrate_until = key.migration_until(&meta);
        Ok(())
    }

    /// Check a typed recovery key against the account's marker and, when it
    /// is the account's key, use it from now on. The caller saves it.
    pub async fn unlock_e2e(&self, key: &SyncKey) -> Result<(), UnlockError> {
        let meta = self
            .get_state_raw(E2E_META_KEY)
            .await
            .map_err(|e| UnlockError::Network(e.to_string()))?;
        let Some(meta) = meta else {
            return Err(UnlockError::NotOn);
        };
        match resolve_status(Some(&meta.value), Some(key)) {
            E2eStatus::On { .. } => {}
            E2eStatus::Unsupported => return Err(UnlockError::Unsupported),
            _ => return Err(UnlockError::WrongKey),
        }
        let mut st = self.e2e_write();
        st.local_key = Some(Arc::new(key.clone()));
        st.status = E2eStatus::On {
            key_id: key.key_id_hex(),
        };
        st.migrate_until = key.migration_until(&meta.value);
        Ok(())
    }
}

/// Why a typed recovery key was not taken.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UnlockError {
    #[error("This recovery key is not the one for this account. Check that it comes from the machine where encryption was turned on.")]
    WrongKey,
    #[error("End-to-end encryption is not on for this account, so no recovery key is needed.")]
    NotOn,
    #[error("This account was encrypted by a newer Mnemos. Update Mnemos on this machine first.")]
    Unsupported,
    #[error("Could not reach the sync server: {0}")]
    Network(String),
}

/// The error a personal call returns while the account cannot be read here.
pub fn locked_error(status: &E2eStatus) -> IdeError {
    let why = match status {
        E2eStatus::WrongKey => "the recovery key saved here is not this account's; enter the right one in Settings > Sync",
        E2eStatus::Unsupported => "this account was encrypted by a newer Mnemos; update Mnemos",
        E2eStatus::MarkerMissing => "a recovery key is saved here but the server has no encryption marker; restore it in Settings > Sync",
        E2eStatus::Unknown => "status not known yet; try again in a moment",
        _ => "on for this account; enter the recovery key in Settings > Sync to sync this machine",
    };
    IdeError::Other(format!("{LOCKED_PREFIX}{why}"))
}

/// Whether `err` is a personal call refused because the account is locked.
pub fn is_locked_error(err: &IdeError) -> bool {
    matches!(err, IdeError::Other(msg) if msg.starts_with(LOCKED_PREFIX))
}

/// `key` as one URL path segment: `%` and `/` escaped (the server decodes
/// the segment back into the key).
pub(crate) fn path_key(key: &str) -> String {
    key.replace('%', "%25").replace('/', "%2F")
}

fn sealed_key(key: &str) -> String {
    format!("{E2E_KEY_PREFIX}{key}")
}

fn sealed_blob(path: &str) -> String {
    format!("{E2E_BLOB_PREFIX}{path}")
}

/// Whether the server timestamp `a` is later than `b`. Unreadable ones
/// count as older, so the encrypted copy wins a tie.
fn newer(a: &str, b: &str) -> bool {
    use mnemos_ide_core::registry::parse_timestamp;
    match (parse_timestamp(a), parse_timestamp(b)) {
        (Some(a), Some(b)) => a > b,
        (Some(_), None) => true,
        _ => false,
    }
}

fn is_platform_envelope(v: &Value) -> bool {
    v.as_object()
        .is_some_and(|o| !o.is_empty() && o.keys().all(|k| PLATFORM_KEYS.contains(&k.as_str())))
}

/// The merge of a plaintext value into the encrypted copy of the same key:
/// the key's own merge with the newer side as "local". A platform envelope
/// is merged per platform, so one machine's slot never reverts another's.
pub(crate) fn merge_absorbed(key: &str, plain: &Value, enc: &Value, plain_newer: bool) -> Value {
    let (newer, older) = if plain_newer {
        (plain, enc)
    } else {
        (enc, plain)
    };
    if is_platform_envelope(newer) && is_platform_envelope(older) {
        let mut out = older.as_object().cloned().unwrap_or_default();
        for (platform, slot) in newer.as_object().into_iter().flatten() {
            let merged = match out.get(platform) {
                Some(old) => merge_for_key(key, slot, old),
                None => slot.clone(),
            };
            out.insert(platform.clone(), merged);
        }
        return Value::Object(out);
    }
    merge_for_key(key, newer, older)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_newer_side_wins_a_whole_value() {
        let merged = merge_absorbed("claude-md", &json!("old build"), &json!("encrypted"), true);
        assert_eq!(merged, json!("old build"));
        let merged = merge_absorbed("claude-md", &json!("old build"), &json!("encrypted"), false);
        assert_eq!(merged, json!("encrypted"));
    }

    #[test]
    fn platform_slots_merge_one_by_one() {
        let plain = json!({"win32": {"a": 1}});
        let enc = json!({"darwin": {"b": 2}, "win32": {"a": 0}});
        let merged = merge_absorbed("claude-settings", &plain, &enc, true);
        assert_eq!(merged, json!({"darwin": {"b": 2}, "win32": {"a": 1}}));
    }

    #[test]
    fn a_key_with_a_slash_is_one_path_segment() {
        assert_eq!(path_key("claude-memory:p:o/r"), "claude-memory:p:o%2Fr");
        assert_eq!(path_key("a%2Fb"), "a%252Fb");
        assert_eq!(path_key("projects"), "projects");
    }

    #[test]
    fn timestamps_decide_which_side_is_newer() {
        assert!(newer("2026-10-09T10:00:01Z", "2026-10-09T10:00:00Z"));
        assert!(!newer("2026-10-09T10:00:00Z", "2026-10-09T10:00:00Z"));
        assert!(!newer("garbage", "2026-10-09T10:00:00Z"));
    }

    #[test]
    fn a_locked_call_is_recognizable() {
        assert!(is_locked_error(&locked_error(&E2eStatus::NeedsKey)));
        assert!(!is_locked_error(&IdeError::Other("x".into())));
    }

    #[tokio::test]
    async fn status_is_learned_from_the_marker_in_a_pull() {
        let c = SyncClient::new(crate::auth::AdminAuth::with_base_url(
            "https://admin.example.test",
        ));
        assert_eq!(c.e2e_status(), E2eStatus::Unknown);
        assert_eq!(c.note_meta(None), E2eStatus::Off);
        let key = SyncKey::generate();
        let meta = key.new_meta("2026-10-09T00:00:00Z");
        assert_eq!(c.note_meta(Some(&meta)), E2eStatus::NeedsKey);
        c.set_local_e2e_key(Some(key.clone()));
        assert_eq!(c.e2e_status(), E2eStatus::Unknown);
        assert_eq!(
            c.note_meta(Some(&meta)),
            E2eStatus::On {
                key_id: key.key_id_hex()
            }
        );
        // Clones share the state.
        assert!(matches!(c.clone().e2e_status(), E2eStatus::On { .. }));
        // The marker gone while encrypting: paused, neither plaintext nor
        // encrypted uploads, and back on once the marker is there again.
        assert_eq!(c.note_meta(None), E2eStatus::MarkerMissing);
        assert!(matches!(c.note_meta(Some(&meta)), E2eStatus::On { .. }));
        // A fresh run with the key saved and no marker: locked, never off.
        let fresh = SyncClient::new(crate::auth::AdminAuth::with_base_url(
            "https://admin.example.test",
        ));
        fresh.set_local_e2e_key(Some(key));
        assert_eq!(fresh.note_meta(None), E2eStatus::MarkerMissing);
        assert!(is_locked_error(&locked_error(&E2eStatus::MarkerMissing)));
    }

    #[test]
    fn the_migration_window_follows_the_sealed_marker() {
        let c = SyncClient::new(crate::auth::AdminAuth::with_base_url(
            "https://admin.example.test",
        ));
        let key = SyncKey::generate();
        c.set_local_e2e_key(Some(key.clone()));
        let now = mnemos_ide_core::registry::format_timestamp(chrono::Utc::now());
        c.note_meta(Some(&key.new_meta(&now)));
        assert!(c.migrating());
        assert!(c.e2e_report().migrating);
        let later = SystemTime::now()
            + std::time::Duration::from_secs(crate::crypto::MIGRATION_WINDOW_SECS as u64 + 60);
        assert!(!c.e2e_read().migrating_at(later));
        c.set_local_e2e_key(Some(key.clone()));
        c.note_meta(Some(&key.new_meta("2020-01-01T00:00:00Z")));
        assert!(!c.migrating());
        c.set_local_e2e_key(Some(key.clone()));
        c.note_meta(Some(&key.new_meta_without_migration(&now)));
        assert!(!c.migrating());
        // A window is only ever read while the status is On.
        c.set_local_e2e_key(None);
        c.note_meta(Some(&key.new_meta(&now)));
        assert_eq!(c.e2e_status(), E2eStatus::NeedsKey);
        assert!(!c.migrating());
    }

    #[test]
    fn an_older_copy_than_one_seen_is_refused() {
        let c = SyncClient::new(crate::auth::AdminAuth::with_base_url(
            "https://admin.example.test",
        ));
        let k = SyncKey::generate();
        let tol = crate::crypto::ROLLBACK_TOLERANCE_MS;
        let newer = k.seal_state_seq("claude-settings", &json!({"hooks": {}}), 10 * tol);
        let older = k.seal_state_seq("claude-settings", &json!({"hooks": {"x": 1}}), 8 * tol);
        assert!(c.open_value(&k, "claude-settings", &newer).is_ok());
        assert_eq!(
            c.open_value(&k, "claude-settings", &older),
            Err(E2eError::Rollback)
        );
        // The same copy again, or one within the tolerance, is fine.
        assert!(c.open_value(&k, "claude-settings", &newer).is_ok());
        let close = k.seal_state_seq("claude-settings", &json!(1), 10 * tol - tol / 2);
        assert!(c.open_value(&k, "claude-settings", &close).is_ok());
        // Other keys are tracked on their own.
        assert!(c
            .open_value(
                &k,
                "claude-md",
                &k.seal_state_seq("claude-md", &json!("a"), 1)
            )
            .is_ok());
        // A write goes past the highest seq seen, even one from a fast clock.
        let ahead = unix_ms(SystemTime::now()) + 24 * 60 * 60 * 1000;
        let fast = k.seal_state_seq("claude-agents", &json!({}), ahead);
        assert!(c.open_value(&k, "claude-agents", &fast).is_ok());
        let (_, seq) = c.seal_next(&k, "claude-agents", &json!({"a": 1}));
        assert!(seq > ahead);
    }

    // ---- Against a fake server ----

    mod fake {
        use std::collections::BTreeMap;
        use std::sync::{Arc, Mutex};

        use serde_json::{json, Value};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        use crate::client::SyncClient;

        pub(super) const SYNCED_AT: &str = "2026-10-09T00:00:00Z";

        #[derive(Default)]
        pub(super) struct Server {
            pub(super) states: BTreeMap<String, (Value, i64, String)>,
            pub(super) blobs: BTreeMap<String, Vec<u8>>,
            pub(super) puts: Vec<String>,
            /// Conditional deletes refused because the value moved on.
            pub(super) conditional_delete_refusals: usize,
            /// Runs once, after the next `GET /state/:key` of that key: a
            /// write by another machine between the absorb's check and its
            /// delete.
            pub(super) after_get: Option<(String, AfterGet)>,
        }

        pub(super) type Shared = Arc<Mutex<Server>>;
        pub(super) type AfterGet = Box<dyn FnOnce(&mut Server) + Send>;

        impl Server {
            pub(super) fn put(&mut self, key: &str, value: Value) {
                let version = self.states.get(key).map_or(1, |s| s.1 + 1);
                self.states
                    .insert(key.to_string(), (value, version, SYNCED_AT.to_string()));
            }
        }

        fn unescape(s: &str) -> String {
            s.replace("%2F", "/").replace("%25", "%")
        }

        fn state_json(key: &str, s: &(Value, i64, String)) -> Value {
            json!({"key": key, "value": s.0, "version": s.1, "synced_at": s.2})
        }

        fn route(srv: &Shared, method: &str, target: &str, body: &[u8]) -> (&'static str, Vec<u8>) {
            let mut st = srv.lock().unwrap();
            let ok_json = |v: Value| ("200 OK", serde_json::to_vec(&v).unwrap());
            let not_found = ("404 Not Found", b"{}".to_vec());
            const STATE: &str = "/api/tcb/state";
            const BLOBS: &str = "/api/tcb/blobs";
            if method == "GET" && target == STATE {
                let states: Vec<Value> = st.states.iter().map(|(k, s)| state_json(k, s)).collect();
                return ok_json(json!({ "states": states }));
            }
            if method == "POST" && target == STATE {
                let body: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
                let key = body["key"].as_str().unwrap_or_default().to_string();
                let current = st.states.get(&key).map_or(0, |s| s.1);
                if let Some(expected) = body["expected_version"].as_i64() {
                    if expected != current {
                        let value = st.states.get(&key).map_or(Value::Null, |s| s.0.clone());
                        return (
                            "409 Conflict",
                            serde_json::to_vec(
                                &json!({"current_version": current, "current_value": value}),
                            )
                            .unwrap(),
                        );
                    }
                }
                st.put(&key, body["value"].clone());
                st.puts.push(key.clone());
                let reply = state_json(&key, &st.states[&key]);
                return ok_json(reply);
            }
            if let Some(rest) = target.strip_prefix("/api/tcb/state/") {
                let (key, query) = rest.split_once('?').unwrap_or((rest, ""));
                let key = unescape(key);
                let expected = query
                    .split('&')
                    .find_map(|kv| kv.strip_prefix("expected_version="))
                    .and_then(|v| v.parse::<i64>().ok());
                let current = st.states.get(&key).map(|s| s.1);
                return match method {
                    "GET" => {
                        let reply = match st.states.get(&key) {
                            Some(s) => ok_json(state_json(&key, s)),
                            None => not_found,
                        };
                        if st.after_get.as_ref().is_some_and(|(k, _)| *k == key) {
                            if let Some((_, after)) = st.after_get.take() {
                                after(&mut st);
                            }
                        }
                        reply
                    }
                    "DELETE" => match (current, expected) {
                        (None, _) => not_found,
                        (Some(current), Some(expected)) if current != expected => {
                            st.conditional_delete_refusals += 1;
                            ("409 Conflict", b"{}".to_vec())
                        }
                        (Some(_), _) => {
                            st.states.remove(&key);
                            ok_json(json!({"success": true}))
                        }
                    },
                    _ => not_found,
                };
            }
            if let Some(query) = target.strip_prefix(&format!("{BLOBS}?")) {
                let prefix = query
                    .split('&')
                    .find_map(|kv| kv.strip_prefix("prefix="))
                    .map(unescape)
                    .unwrap_or_default();
                let items: Vec<Value> = st
                    .blobs
                    .iter()
                    .filter(|(p, _)| p.starts_with(&prefix))
                    .map(|(p, b)| json!({"path": p, "size": b.len(), "etag": "e", "lastModified": SYNCED_AT}))
                    .collect();
                return ok_json(json!({ "items": items }));
            }
            if let Some(path) = target.strip_prefix(&format!("{BLOBS}/")) {
                let path = unescape(path);
                return match method {
                    "GET" => match st.blobs.get(&path) {
                        Some(b) => ("200 OK", b.clone()),
                        None => not_found,
                    },
                    "PUT" => {
                        st.blobs.insert(path, body.to_vec());
                        ok_json(json!({}))
                    }
                    "DELETE" => {
                        st.blobs.remove(&path);
                        ok_json(json!({}))
                    }
                    _ => not_found,
                };
            }
            not_found
        }

        async fn serve_one(mut sock: tokio::net::TcpStream, srv: Shared) -> std::io::Result<()> {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            let head_end = loop {
                let n = sock.read(&mut chunk).await?;
                if n == 0 {
                    return Ok(());
                }
                buf.extend_from_slice(&chunk[..n]);
                if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break i + 4;
                }
            };
            let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
            let len: usize = head
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.trim()
                        .eq_ignore_ascii_case("content-length")
                        .then(|| v.trim().parse().ok())
                        .flatten()
                })
                .unwrap_or(0);
            while buf.len() < head_end + len {
                let n = sock.read(&mut chunk).await?;
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            let mut first = head.lines().next().unwrap_or_default().split_whitespace();
            let method = first.next().unwrap_or_default().to_string();
            let target = first.next().unwrap_or_default().to_string();
            let (status, bytes) = route(&srv, &method, &target, &buf[head_end..]);
            let headers = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            );
            sock.write_all(headers.as_bytes()).await?;
            sock.write_all(&bytes).await?;
            sock.shutdown().await
        }

        /// A client signed in to a fake server holding `srv`.
        pub(super) async fn client(srv: &Shared) -> SyncClient {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let srv = srv.clone();
            tokio::spawn(async move {
                while let Ok((sock, _)) = listener.accept().await {
                    let srv = srv.clone();
                    tokio::spawn(async move {
                        let _ = serve_one(sock, srv).await;
                    });
                }
            });
            SyncClient::new(crate::auth::AdminAuth::from_tokens(
                format!("http://{addr}"),
                crate::auth::AdminTokens {
                    access_token: "AT".to_string(),
                    refresh_token: "RT".to_string(),
                    expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
                },
            ))
        }
    }

    fn now_text() -> String {
        mnemos_ide_core::registry::format_timestamp(chrono::Utc::now())
    }

    #[tokio::test]
    async fn a_saved_key_without_a_marker_never_uploads_plaintext() {
        let srv = fake::Shared::default();
        srv.lock().unwrap().put("claude-md", json!("server copy"));
        let c = fake::client(&srv).await;
        c.set_local_e2e_key(Some(SyncKey::generate()));

        // A single call resolves the status with a GET of the marker.
        let err = c
            .put_state("claude-md", &json!("local notes"), None)
            .await
            .unwrap_err();
        assert!(is_locked_error(&err), "{err}");
        assert_eq!(c.e2e_status(), E2eStatus::MarkerMissing);
        let err = c.get_state("claude-md").await.unwrap_err();
        assert!(is_locked_error(&err), "{err}");
        let err = c
            .put_blob("claude-sessions/s/m.meta.json", b"{}".to_vec())
            .await
            .unwrap_err();
        assert!(is_locked_error(&err), "{err}");
        // A full pull hides personal keys like a missing key does.
        let states = c.pull_all().await.unwrap();
        assert!(
            states.iter().all(|s| !is_encryptable_key(&s.key)),
            "{states:?}"
        );
        assert_eq!(c.e2e_status(), E2eStatus::MarkerMissing);
        // Nothing personal went up; the live feed still does.
        assert!(srv.lock().unwrap().puts.is_empty());
        assert!(srv.lock().unwrap().blobs.is_empty());
        c.put_state("agents-live:mac", &json!({"machine": "mac"}), None)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn restoring_the_marker_keeps_the_key_and_opens_no_migration_window() {
        let srv = fake::Shared::default();
        srv.lock()
            .unwrap()
            .put("claude-md", json!("planted by the server"));
        let c = fake::client(&srv).await;
        let key = SyncKey::generate();
        c.set_local_e2e_key(Some(key.clone()));
        assert_eq!(c.resolve_e2e().await.unwrap(), E2eStatus::MarkerMissing);

        c.enable_e2e(&key).await.unwrap();
        assert!(matches!(c.e2e_status(), E2eStatus::On { .. }));
        let meta = srv.lock().unwrap().states[E2E_META_KEY].0.clone();
        assert!(key.matches_meta(&meta).is_ok());
        assert_eq!(key.migration_until(&meta), None);
        assert!(!c.migrating());

        // Plaintext is not taken in: not returned, not encrypted, not deleted.
        let states = c.pull_all().await.unwrap();
        assert!(states.iter().all(|s| s.key != "claude-md"), "{states:?}");
        assert!(srv.lock().unwrap().states.contains_key("claude-md"));
        assert!(!srv.lock().unwrap().states.contains_key("e2e:claude-md"));
        assert_eq!(c.e2e_report().ignored, vec!["claude-md".to_string()]);
    }

    #[tokio::test]
    async fn plaintext_after_the_migration_window_is_ignored() {
        let srv = fake::Shared::default();
        let key = SyncKey::generate();
        {
            let mut s = srv.lock().unwrap();
            s.put(E2E_META_KEY, key.new_meta("2020-01-01T00:00:00Z"));
            s.put("e2e:claude-md", key.seal_state("claude-md", &json!("mine")));
            // A newer-looking plaintext value: the server wants it applied.
            s.put("claude-md", json!("injected"));
            s.states.get_mut("claude-md").unwrap().2 = "2099-01-01T00:00:00Z".into();
            s.put("claude-agents", json!({"evil.md": "x"}));
            s.blobs.insert(
                "claude-sessions/s/m-000001.jsonl.gz".into(),
                b"plain".to_vec(),
            );
        }
        let c = fake::client(&srv).await;
        c.set_local_e2e_key(Some(key.clone()));

        let states = c.pull_all().await.unwrap();
        let md = states.iter().find(|s| s.key == "claude-md").unwrap();
        assert_eq!(md.value, json!("mine"));
        assert!(
            states.iter().all(|s| s.key != "claude-agents"),
            "{states:?}"
        );
        let report = c.e2e_report();
        assert_eq!(report.absorbed, 0);
        assert_eq!(
            report.ignored,
            vec!["claude-agents".to_string(), "claude-md".to_string()]
        );
        {
            let s = srv.lock().unwrap();
            // Left alone: neither sealed under the key nor deleted.
            assert!(!s.states.contains_key("e2e:claude-agents"));
            assert!(s.states.contains_key("claude-agents"));
            assert!(s.states.contains_key("claude-md"));
        }

        // Transcripts: no plaintext fallback, plaintext not listed.
        assert_eq!(
            c.get_blob("claude-sessions/s/m-000001.jsonl.gz")
                .await
                .unwrap(),
            None
        );
        let listed = c.list_blobs("claude-sessions/").await.unwrap();
        assert!(listed.is_empty(), "{listed:?}");
        assert!(srv
            .lock()
            .unwrap()
            .blobs
            .contains_key("claude-sessions/s/m-000001.jsonl.gz"));
    }

    #[tokio::test]
    async fn plaintext_inside_the_migration_window_is_absorbed() {
        let srv = fake::Shared::default();
        let key = SyncKey::generate();
        {
            let mut s = srv.lock().unwrap();
            s.put(E2E_META_KEY, key.new_meta(&now_text()));
            s.put("claude-md", json!("from before"));
            s.blobs.insert(
                "claude-sessions/s/m-000001.jsonl.gz".into(),
                b"plain".to_vec(),
            );
        }
        let c = fake::client(&srv).await;
        c.set_local_e2e_key(Some(key.clone()));
        let states = c.pull_all().await.unwrap();
        let md = states.iter().find(|s| s.key == "claude-md").unwrap();
        assert_eq!(md.value, json!("from before"));
        let listed = c.list_blobs("claude-sessions/").await.unwrap();
        assert_eq!(listed.len(), 1);
        let s = srv.lock().unwrap();
        assert!(!s.states.contains_key("claude-md"));
        assert!(s.states.contains_key("e2e:claude-md"));
        assert!(!s.blobs.contains_key("claude-sessions/s/m-000001.jsonl.gz"));
        assert!(s
            .blobs
            .contains_key("e2e/claude-sessions/s/m-000001.jsonl.gz"));
        assert!(c.e2e_report().ignored.is_empty());
    }

    /// Crypto finding 6: an older build writes the plaintext value after the
    /// absorb checked its version. The conditional delete is refused, so
    /// that write survives for the next pull to absorb.
    #[tokio::test]
    async fn a_plaintext_write_racing_the_absorb_is_not_deleted() {
        let srv = fake::Shared::default();
        let key = SyncKey::generate();
        {
            let mut s = srv.lock().unwrap();
            s.put(E2E_META_KEY, key.new_meta(&now_text()));
            s.put("claude-md", json!("from before"));
            // Written after the absorb read it, so newer than its copy.
            let write = |s: &mut fake::Server| {
                s.states.insert(
                    "claude-md".into(),
                    (
                        json!("old build wrote this"),
                        2,
                        "2026-10-10T00:00:00Z".into(),
                    ),
                );
            };
            s.after_get = Some((
                "claude-md".to_string(),
                Box::new(write) as Box<dyn FnOnce(&mut fake::Server) + Send>,
            ));
        }
        let c = fake::client(&srv).await;
        c.set_local_e2e_key(Some(key.clone()));
        c.pull_all().await.unwrap();
        {
            let s = srv.lock().unwrap();
            assert_eq!(s.conditional_delete_refusals, 1);
            assert_eq!(s.states["claude-md"].0, json!("old build wrote this"));
            assert!(s.states.contains_key("e2e:claude-md"));
        }
        // The next pull takes it in.
        let states = c.pull_all().await.unwrap();
        let md = states.iter().find(|s| s.key == "claude-md").unwrap();
        assert_eq!(md.value, json!("old build wrote this"));
        assert!(!srv.lock().unwrap().states.contains_key("claude-md"));
    }

    #[tokio::test]
    async fn a_replayed_older_value_is_not_applied() {
        let srv = fake::Shared::default();
        let key = SyncKey::generate();
        let now = unix_ms(SystemTime::now());
        let old = key.seal_state_seq(
            "claude-settings",
            &json!({"hook": "removed"}),
            now - 3_600_000,
        );
        {
            let mut s = srv.lock().unwrap();
            s.put(E2E_META_KEY, key.new_meta("2020-01-01T00:00:00Z"));
            s.put(
                "e2e:claude-settings",
                key.seal_state_seq("claude-settings", &json!({}), now),
            );
        }
        let c = fake::client(&srv).await;
        c.set_local_e2e_key(Some(key.clone()));
        let states = c.pull_all().await.unwrap();
        assert!(states.iter().any(|s| s.key == "claude-settings"));

        // The server hands back the old copy.
        srv.lock().unwrap().put("e2e:claude-settings", old);
        let states = c.pull_all().await.unwrap();
        assert!(
            states.iter().all(|s| s.key != "claude-settings"),
            "{states:?}"
        );
        assert_eq!(
            c.e2e_report().unreadable,
            vec!["claude-settings".to_string()]
        );
        let err = c.get_state("claude-settings").await.unwrap_err();
        assert!(err.to_string().contains("older copy"), "{err}");

        // A real write goes past it and reads back.
        c.put_state("claude-settings", &json!({"new": true}), None)
            .await
            .unwrap();
        let got = c.get_state("claude-settings").await.unwrap().unwrap();
        assert_eq!(got.value, json!({"new": true}));
    }
}
