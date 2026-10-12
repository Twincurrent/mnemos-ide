// SPDX-License-Identifier: AGPL-3.0-only
//! End-to-end encryption of personal sync data.
//!
//! Turned on per admin account by an explicit action in Settings > Sync. The
//! first machine generates a random 256-bit key and shows it once as a
//! recovery key; every other machine needs it typed in. The key never leaves
//! the machine except as that text: the server only ever sees ciphertext,
//! plus a small marker ([`E2E_META_KEY`]) that says encryption is on and lets
//! a typed key be checked.
//!
//! What is encrypted: every personal state key (see [`is_encryptable_key`])
//! and every transcript blob (see [`is_encryptable_blob`]). What is not: the
//! team scope (server-readable by decision), the live feed
//! (`agents-live:*`, read by other apps by its documented shape) and the
//! marker itself.
//!
//! Encrypted values live in their own namespace (`e2e:<key>` for state,
//! `e2e/<path>` for blobs), never under the plaintext names. A build from
//! before this change ignores state keys it does not know and lists blobs
//! under `claude-sessions/` only, so it never receives ciphertext it could
//! write to disk as if it were a file.
//!
//! Envelope (state value, JSON):
//! `{"mnemosE2e":2,"kid":"<16 hex>","seq":<u64>,"nonce":"<b64 24 bytes>","ct":"<b64>","tag":"<b64 16 bytes>"}`
//! (version 1, without `seq`, is still read; it counts as `seq` 0).
//!
//! Envelope (blob, bytes): `MNE2` | version (1 byte) | key id (8 bytes) |
//! nonce (24 bytes) | ciphertext | tag (16 bytes).
//!
//! The cipher is XChaCha20-Poly1305 with a random 192-bit nonce per value.
//! The associated data binds the envelope version, the key id and the
//! logical key name (or blob path), so a value cannot be replayed under
//! another key or path. The AEAD key and the key id are both derived from
//! the recovery key with HKDF-SHA256 under separate labels.
//!
//! Rollback: a state envelope also binds `seq`, the writer's clock in
//! milliseconds, raised past the highest `seq` the writer had seen for that
//! key. The client remembers the highest `seq` it opened per key during a
//! run and refuses one more than [`ROLLBACK_TOLERANCE_MS`] older, so a
//! server cannot hand back an old copy (a removed hook, say) once a newer
//! one was seen. Limits: the high-water mark lives in memory, so the first
//! value seen after a start is trusted; the tolerance lets a replay younger
//! than it through (it absorbs clock skew between machines); blobs carry no
//! `seq` (transcript segments are written once per path).
//!
//! Integrity against the server: while encryption is on, plaintext found
//! under a personal name is only taken during the migration window that the
//! marker carries, sealed with the key ([`SyncKey::migration_until`]), so a
//! server cannot open one. Outside it plaintext is ignored.

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use rand::RngCore;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Prefix of an encrypted personal state key on the server.
pub const E2E_KEY_PREFIX: &str = "e2e:";
/// Prefix of an encrypted blob path on the server.
pub const E2E_BLOB_PREFIX: &str = "e2e/";
/// Plaintext state key that marks the account as encrypted. Holds the key id
/// and a sealed check value, never the key.
pub const E2E_META_KEY: &str = "e2e-meta";
/// Blob paths under this prefix are transcripts, which are encrypted.
pub const TRANSCRIPT_BLOB_PREFIX: &str = "claude-sessions/";

const ENVELOPE_VERSION: u8 = 1;
/// State envelopes carry `seq` from this version on.
const STATE_ENVELOPE_VERSION: u8 = 2;
/// How long after encryption is turned on plaintext personal values are
/// still absorbed (an older build's last writes, the initial migration).
pub const MIGRATION_WINDOW_SECS: i64 = 24 * 60 * 60;
/// How much older than the newest copy seen here a state value may be
/// before it is refused as a replay. Covers clock skew between machines.
pub const ROLLBACK_TOLERANCE_MS: u64 = 5 * 60 * 1000;
const BLOB_MAGIC: &[u8; 4] = b"MNE2";
const KEY_LEN: usize = 32;
const KEY_ID_LEN: usize = 8;
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;
const CHECKSUM_LEN: usize = 2;
/// Characters in a recovery key, without separators: 34 bytes in base32.
const RECOVERY_CHARS: usize = 55;
const RECOVERY_GROUP: usize = 5;
const BASE32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
const CHECK_PLAINTEXT: &[u8] = b"mnemos sync e2e key check v1";

/// Whether a personal state key is encrypted when end-to-end encryption is
/// on. The live feed stays readable by other apps (its key scheme is a
/// published contract), `twincurrent-build:*` is a legacy server-side key,
/// and the encrypted namespace and the marker are not data.
pub fn is_encryptable_key(key: &str) -> bool {
    !(key.starts_with(E2E_KEY_PREFIX)
        || key == E2E_META_KEY
        || key.starts_with(crate::live_agents::KEY_PREFIX)
        || key.starts_with("twincurrent-build:"))
}

/// Whether a blob path is encrypted when end-to-end encryption is on.
pub fn is_encryptable_blob(path: &str) -> bool {
    path.starts_with(TRANSCRIPT_BLOB_PREFIX)
}

/// Why a value could not be decrypted, or a recovery key not used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum E2eError {
    #[error("not an encrypted value")]
    NotAnEnvelope,
    #[error("encrypted with an unsupported format version {0}")]
    UnsupportedVersion(u64),
    #[error("encrypted with another key (key id {0})")]
    OtherKey(String),
    #[error("the encrypted value was changed or does not belong here")]
    Tampered,
    #[error("an older copy than one already read on this machine (possible replay)")]
    Rollback,
}

/// What was wrong with a typed recovery key.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RecoveryKeyError {
    #[error("A recovery key has 55 letters and digits; this one has {0}. Check that you copied all of it.")]
    WrongLength(usize),
    #[error(
        "A recovery key only uses the letters A-Z and the digits 2-7; '{0}' is not one of them."
    )]
    BadCharacter(char),
    #[error("This is not a valid recovery key. Check it for typos.")]
    Checksum,
}

/// The account's sync key. Holds key material; `Debug` never prints it.
#[derive(Clone)]
pub struct SyncKey {
    master: [u8; KEY_LEN],
    aead_key: [u8; KEY_LEN],
    key_id: [u8; KEY_ID_LEN],
}

impl std::fmt::Debug for SyncKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncKey")
            .field("key_id", &self.key_id_hex())
            .finish_non_exhaustive()
    }
}

impl PartialEq for SyncKey {
    fn eq(&self, other: &Self) -> bool {
        self.master == other.master
    }
}

impl SyncKey {
    /// A new random key from the OS random number generator.
    pub fn generate() -> Self {
        let mut master = [0u8; KEY_LEN];
        rand::rngs::OsRng.fill_bytes(&mut master);
        Self::from_master(master)
    }

    fn from_master(master: [u8; KEY_LEN]) -> Self {
        let hk = Hkdf::<Sha256>::new(Some(b"mnemos-sync-e2e"), &master);
        let mut aead_key = [0u8; KEY_LEN];
        let mut key_id = [0u8; KEY_ID_LEN];
        // Both lengths are far below HKDF-SHA256's limit (255 * 32).
        hk.expand(b"v1 aead key", &mut aead_key)
            .expect("hkdf output length is valid");
        hk.expand(b"v1 key id", &mut key_id)
            .expect("hkdf output length is valid");
        Self {
            master,
            aead_key,
            key_id,
        }
    }

    /// The public id of this key, as stored next to every value it seals.
    pub fn key_id_hex(&self) -> String {
        hex(&self.key_id)
    }

    /// The recovery key the user writes down: the key plus a 2-byte
    /// checksum in base32, in groups of five separated by `-`.
    pub fn recovery_text(&self) -> String {
        let mut bytes = self.master.to_vec();
        bytes.extend_from_slice(&checksum(&self.master));
        let chars = base32_encode(&bytes);
        chars
            .as_bytes()
            .chunks(RECOVERY_GROUP)
            .map(|c| std::str::from_utf8(c).unwrap_or_default())
            .collect::<Vec<_>>()
            .join("-")
    }

    /// Parse a recovery key as typed: case, spaces, dashes and line breaks
    /// do not matter, and the digits 0, 1 and 8 are read as O, I and B.
    pub fn parse_recovery(text: &str) -> Result<Self, RecoveryKeyError> {
        let mut cleaned = String::with_capacity(RECOVERY_CHARS);
        for c in text.chars() {
            if c.is_whitespace() || c == '-' {
                continue;
            }
            let c = match c.to_ascii_uppercase() {
                '0' => 'O',
                '1' => 'I',
                '8' => 'B',
                other => other,
            };
            if !BASE32.contains(&(c as u32 as u8)) || !c.is_ascii() {
                return Err(RecoveryKeyError::BadCharacter(c));
            }
            cleaned.push(c);
        }
        if cleaned.len() != RECOVERY_CHARS {
            return Err(RecoveryKeyError::WrongLength(cleaned.len()));
        }
        let bytes = base32_decode(&cleaned).ok_or(RecoveryKeyError::Checksum)?;
        if bytes.len() != KEY_LEN + CHECKSUM_LEN {
            return Err(RecoveryKeyError::Checksum);
        }
        let mut master = [0u8; KEY_LEN];
        master.copy_from_slice(&bytes[..KEY_LEN]);
        if bytes[KEY_LEN..] != checksum(&master) {
            return Err(RecoveryKeyError::Checksum);
        }
        Ok(Self::from_master(master))
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new(Key::from_slice(&self.aead_key))
    }

    fn seal(&self, aad: &[u8], msg: &[u8]) -> ([u8; NONCE_LEN], Vec<u8>) {
        let mut nonce = [0u8; NONCE_LEN];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let out = self
            .cipher()
            .encrypt(XNonce::from_slice(&nonce), Payload { msg, aad })
            // XChaCha20-Poly1305 only fails for messages near 256 GiB.
            .expect("xchacha20poly1305 encryption cannot fail for sync-sized values");
        (nonce, out)
    }

    fn open(&self, aad: &[u8], nonce: &[u8], ct_and_tag: &[u8]) -> Result<Vec<u8>, E2eError> {
        if nonce.len() != NONCE_LEN || ct_and_tag.len() < TAG_LEN {
            return Err(E2eError::Tampered);
        }
        self.cipher()
            .decrypt(
                XNonce::from_slice(nonce),
                Payload {
                    msg: ct_and_tag,
                    aad,
                },
            )
            .map_err(|_| E2eError::Tampered)
    }

    /// Encrypt a state value for the logical key `key`, with this
    /// machine's clock as `seq`.
    pub fn seal_state(&self, key: &str, value: &Value) -> Value {
        self.seal_state_seq(key, value, unix_ms(std::time::SystemTime::now()))
    }

    /// Encrypt a state value for the logical key `key` with `seq` bound in
    /// the associated data (see the module docs on rollback).
    pub fn seal_state_seq(&self, key: &str, value: &Value, seq: u64) -> Value {
        let msg = serde_json::to_vec(value).unwrap_or_default();
        let aad = state_associated_data(&self.key_id, key, seq);
        let (nonce, mut out) = self.seal(&aad, &msg);
        let tag = out.split_off(out.len() - TAG_LEN);
        json!({
            "mnemosE2e": STATE_ENVELOPE_VERSION,
            "kid": self.key_id_hex(),
            "seq": seq,
            "nonce": B64.encode(nonce),
            "ct": B64.encode(out),
            "tag": B64.encode(tag),
        })
    }

    /// Decrypt a state envelope stored for the logical key `key`.
    pub fn open_state(&self, key: &str, envelope: &Value) -> Result<Value, E2eError> {
        self.open_state_seq(key, envelope).map(|(value, _)| value)
    }

    /// [`Self::open_state`], also returning the envelope's authenticated
    /// `seq` (0 for a version 1 envelope, which has none).
    pub fn open_state_seq(&self, key: &str, envelope: &Value) -> Result<(Value, u64), E2eError> {
        let obj = envelope.as_object().ok_or(E2eError::NotAnEnvelope)?;
        let version = obj
            .get("mnemosE2e")
            .and_then(Value::as_u64)
            .ok_or(E2eError::NotAnEnvelope)?;
        let seq = match version {
            v if v == u64::from(ENVELOPE_VERSION) => None,
            v if v == u64::from(STATE_ENVELOPE_VERSION) => Some(
                obj.get("seq")
                    .and_then(Value::as_u64)
                    .ok_or(E2eError::NotAnEnvelope)?,
            ),
            other => return Err(E2eError::UnsupportedVersion(other)),
        };
        let field = |name: &str| -> Result<Vec<u8>, E2eError> {
            let text = obj
                .get(name)
                .and_then(Value::as_str)
                .ok_or(E2eError::NotAnEnvelope)?;
            B64.decode(text).map_err(|_| E2eError::NotAnEnvelope)
        };
        let kid = obj
            .get("kid")
            .and_then(Value::as_str)
            .ok_or(E2eError::NotAnEnvelope)?;
        if kid != self.key_id_hex() {
            return Err(E2eError::OtherKey(kid.to_string()));
        }
        let nonce = field("nonce")?;
        let mut ct = field("ct")?;
        let tag = field("tag")?;
        if tag.len() != TAG_LEN {
            return Err(E2eError::Tampered);
        }
        ct.extend_from_slice(&tag);
        let aad = match seq {
            None => associated_data("state", &self.key_id, key),
            Some(seq) => state_associated_data(&self.key_id, key, seq),
        };
        let plain = self.open(&aad, &nonce, &ct)?;
        let value = serde_json::from_slice(&plain).map_err(|_| E2eError::Tampered)?;
        Ok((value, seq.unwrap_or(0)))
    }

    /// Encrypt a blob stored at the logical path `path`.
    pub fn seal_blob(&self, path: &str, bytes: &[u8]) -> Vec<u8> {
        let aad = associated_data("blob", &self.key_id, path);
        let (nonce, ct) = self.seal(&aad, bytes);
        let mut out = Vec::with_capacity(BLOB_HEADER_LEN + ct.len());
        out.extend_from_slice(BLOB_MAGIC);
        out.push(ENVELOPE_VERSION);
        out.extend_from_slice(&self.key_id);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        out
    }

    /// Decrypt a blob stored at the logical path `path`.
    pub fn open_blob(&self, path: &str, bytes: &[u8]) -> Result<Vec<u8>, E2eError> {
        if bytes.len() < BLOB_HEADER_LEN + TAG_LEN || &bytes[..4] != BLOB_MAGIC {
            return Err(E2eError::NotAnEnvelope);
        }
        if bytes[4] != ENVELOPE_VERSION {
            return Err(E2eError::UnsupportedVersion(u64::from(bytes[4])));
        }
        let kid = &bytes[5..5 + KEY_ID_LEN];
        if kid != self.key_id {
            return Err(E2eError::OtherKey(hex(kid)));
        }
        let nonce = &bytes[5 + KEY_ID_LEN..BLOB_HEADER_LEN];
        let aad = associated_data("blob", &self.key_id, path);
        self.open(&aad, nonce, &bytes[BLOB_HEADER_LEN..])
    }

    /// The marker written to [`E2E_META_KEY`] when encryption is turned on:
    /// the key id, a sealed known value that proves a typed key right, and
    /// the sealed end of the migration window ([`MIGRATION_WINDOW_SECS`]
    /// after `created_at`, an RFC 3339 time; from now when unreadable).
    pub fn new_meta(&self, created_at: &str) -> Value {
        let start = mnemos_ide_core::registry::parse_timestamp(created_at)
            .unwrap_or_else(chrono::Utc::now)
            .timestamp();
        self.meta(created_at, Some(start + MIGRATION_WINDOW_SECS))
    }

    /// A marker with no migration window: plaintext is never absorbed. Used
    /// to put a vanished marker back with the key already in use here.
    pub fn new_meta_without_migration(&self, created_at: &str) -> Value {
        self.meta(created_at, None)
    }

    fn meta(&self, created_at: &str, migrate_until: Option<i64>) -> Value {
        let aad = associated_data("check", &self.key_id, E2E_META_KEY);
        let (nonce, ct) = self.seal(&aad, CHECK_PLAINTEXT);
        let mut check = nonce.to_vec();
        check.extend_from_slice(&ct);
        let mut meta = json!({
            "mnemosE2e": ENVELOPE_VERSION,
            "kid": self.key_id_hex(),
            "check": B64.encode(check),
            "createdAt": created_at,
        });
        if let Some(until) = migrate_until {
            let aad = associated_data("migration", &self.key_id, E2E_META_KEY);
            let msg = serde_json::to_vec(&json!({ "until": until })).unwrap_or_default();
            let (nonce, ct) = self.seal(&aad, &msg);
            let mut sealed = nonce.to_vec();
            sealed.extend_from_slice(&ct);
            meta["migration"] = json!(B64.encode(sealed));
        }
        meta
    }

    /// When the marker's migration window ends (unix seconds), if it has
    /// one sealed with this key. A window the server added or changed does
    /// not open, so it reads as none.
    pub fn migration_until(&self, meta: &Value) -> Option<i64> {
        let sealed = B64.decode(meta.get("migration")?.as_str()?).ok()?;
        if sealed.len() < NONCE_LEN + TAG_LEN {
            return None;
        }
        let aad = associated_data("migration", &self.key_id, E2E_META_KEY);
        let plain = self
            .open(&aad, &sealed[..NONCE_LEN], &sealed[NONCE_LEN..])
            .ok()?;
        serde_json::from_slice::<Value>(&plain)
            .ok()?
            .get("until")?
            .as_i64()
    }

    /// Whether this key is the one the account's marker was made with.
    pub fn matches_meta(&self, meta: &Value) -> Result<(), MetaMismatch> {
        let parsed = parse_meta(meta).ok_or(MetaMismatch::Unsupported)?;
        if parsed.kid != self.key_id_hex() {
            return Err(MetaMismatch::WrongKey);
        }
        if parsed.check.len() < NONCE_LEN + TAG_LEN {
            return Err(MetaMismatch::Unsupported);
        }
        let aad = associated_data("check", &self.key_id, E2E_META_KEY);
        match self.open(&aad, &parsed.check[..NONCE_LEN], &parsed.check[NONCE_LEN..]) {
            Ok(plain) if plain == CHECK_PLAINTEXT => Ok(()),
            _ => Err(MetaMismatch::WrongKey),
        }
    }
}

const BLOB_HEADER_LEN: usize = 4 + 1 + KEY_ID_LEN + NONCE_LEN;

/// Why a key cannot be used with an account's marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetaMismatch {
    /// The marker was made with another key.
    WrongKey,
    /// The marker is in a format this build does not know (a newer build
    /// wrote it, or it is damaged).
    Unsupported,
}

struct ParsedMeta {
    kid: String,
    check: Vec<u8>,
}

fn parse_meta(meta: &Value) -> Option<ParsedMeta> {
    let obj = meta.as_object()?;
    if obj.get("mnemosE2e")?.as_u64()? != u64::from(ENVELOPE_VERSION) {
        return None;
    }
    let kid = obj.get("kid")?.as_str()?.to_string();
    let check = B64.decode(obj.get("check")?.as_str()?).ok()?;
    Some(ParsedMeta { kid, check })
}

/// Where this machine stands with an account's encryption.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum E2eStatus {
    /// Not known yet: no request has told this run whether the account is
    /// encrypted.
    #[default]
    Unknown,
    /// The account is not encrypted. Sync works as before.
    Off,
    /// The account is encrypted and this machine has its key.
    On { key_id: String },
    /// The account is encrypted and this machine has no key: personal sync
    /// is paused until the recovery key is entered.
    NeedsKey,
    /// The account is encrypted and the key saved here is not its key.
    WrongKey,
    /// The account's marker is in a format this build does not read. Update
    /// Mnemos.
    Unsupported,
    /// This machine has a key saved for the account but the server has no
    /// marker. Never read as "off": that would upload personal data in
    /// plaintext because a server dropped one value. Personal sync is paused
    /// until the marker is put back with the saved key (Settings > Sync).
    MarkerMissing,
}

impl E2eStatus {
    /// Personal state and transcripts cannot be read or written.
    pub fn is_locked(&self) -> bool {
        matches!(
            self,
            Self::NeedsKey | Self::WrongKey | Self::Unsupported | Self::MarkerMissing
        )
    }
}

/// The status a marker (`None`: the account has none) and the key saved on
/// this machine add up to. No marker is `Off` only when no key is saved
/// here; with one it is [`E2eStatus::MarkerMissing`].
pub fn resolve_status(meta: Option<&Value>, local: Option<&SyncKey>) -> E2eStatus {
    let Some(meta) = meta else {
        return match local {
            None => E2eStatus::Off,
            Some(_) => E2eStatus::MarkerMissing,
        };
    };
    if parse_meta(meta).is_none() {
        return E2eStatus::Unsupported;
    }
    match local {
        None => E2eStatus::NeedsKey,
        Some(key) => match key.matches_meta(meta) {
            Ok(()) => E2eStatus::On {
                key_id: key.key_id_hex(),
            },
            Err(MetaMismatch::WrongKey) => E2eStatus::WrongKey,
            Err(MetaMismatch::Unsupported) => E2eStatus::Unsupported,
        },
    }
}

fn associated_data(kind: &str, key_id: &[u8; KEY_ID_LEN], name: &str) -> Vec<u8> {
    let mut aad = Vec::with_capacity(32 + name.len());
    aad.extend_from_slice(b"mnemos-e2e/");
    aad.push(ENVELOPE_VERSION);
    aad.push(b'/');
    aad.extend_from_slice(kind.as_bytes());
    aad.push(0);
    aad.extend_from_slice(key_id);
    aad.push(0);
    aad.extend_from_slice(name.as_bytes());
    aad
}

/// Associated data of a version 2 state envelope: version 1's, with the
/// version byte raised and `seq` appended.
fn state_associated_data(key_id: &[u8; KEY_ID_LEN], name: &str, seq: u64) -> Vec<u8> {
    let mut aad = associated_data("state", key_id, name);
    aad[b"mnemos-e2e/".len()] = STATE_ENVELOPE_VERSION;
    aad.push(0);
    aad.extend_from_slice(&seq.to_be_bytes());
    aad
}

/// `t` as milliseconds since the unix epoch (0 before it).
pub fn unix_ms(t: std::time::SystemTime) -> u64 {
    t.duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Whether a state value sealed with `seq` is a rollback, given the highest
/// `seq` already opened for its key on this machine.
pub fn is_rollback(seen: Option<u64>, seq: u64) -> bool {
    seen.is_some_and(|seen| seq.saturating_add(ROLLBACK_TOLERANCE_MS) < seen)
}

fn checksum(master: &[u8; KEY_LEN]) -> [u8; CHECKSUM_LEN] {
    let mut h = Sha256::new();
    h.update(b"mnemos-recovery-key");
    h.update(master);
    let out = h.finalize();
    [out[0], out[1]]
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn base32_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 8 / 5 + 1);
    let mut buffer: u32 = 0;
    let mut bits = 0u32;
    for &b in bytes {
        buffer = (buffer << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(BASE32[((buffer >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(BASE32[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// Inverse of [`base32_encode`]. `None` when the padding bits of the last
/// character are not zero (a typo there would otherwise go unnoticed).
fn base32_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 5 / 8);
    let mut buffer: u32 = 0;
    let mut bits = 0u32;
    for c in text.bytes() {
        let v = BASE32.iter().position(|&a| a == c)? as u32;
        buffer = (buffer << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    if bits > 0 && (buffer & ((1 << bits) - 1)) != 0 {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> SyncKey {
        SyncKey::from_master([byte; KEY_LEN])
    }

    #[test]
    fn state_envelope_round_trips() {
        let k = SyncKey::generate();
        let value = json!({"a": [1, 2, {"b": "c"}], "s": "text"});
        let sealed = k.seal_state("claude-agents", &value);
        assert_eq!(sealed["mnemosE2e"], json!(2));
        assert!(sealed["seq"].as_u64().unwrap() > 0);
        assert_eq!(sealed["kid"], json!(k.key_id_hex()));
        assert!(!sealed.to_string().contains("text"));
        assert_eq!(k.open_state("claude-agents", &sealed).unwrap(), value);
        // A fresh nonce every time.
        assert_ne!(k.seal_state("claude-agents", &value), sealed);
    }

    #[test]
    fn a_state_value_moved_to_another_key_is_rejected() {
        let k = key(1);
        let sealed = k.seal_state("claude-md", &json!("x"));
        assert_eq!(k.open_state("claude-env", &sealed), Err(E2eError::Tampered));
    }

    #[test]
    fn tampered_state_is_rejected() {
        let k = key(2);
        let sealed = k.seal_state("settings", &json!({"x": 1}));
        for field in ["ct", "tag", "nonce"] {
            let mut bad = sealed.clone();
            let mut bytes = B64.decode(bad[field].as_str().unwrap()).unwrap();
            bytes[0] ^= 1;
            bad[field] = json!(B64.encode(bytes));
            assert_eq!(
                k.open_state("settings", &bad),
                Err(E2eError::Tampered),
                "{field}"
            );
        }
        let mut other_version = sealed.clone();
        other_version["mnemosE2e"] = json!(3);
        assert_eq!(
            k.open_state("settings", &other_version),
            Err(E2eError::UnsupportedVersion(3))
        );
        assert_eq!(
            k.open_state("settings", &json!({"plain": true})),
            Err(E2eError::NotAnEnvelope)
        );
        assert!(matches!(
            key(3).open_state("settings", &sealed),
            Err(E2eError::OtherKey(_))
        ));
    }

    #[test]
    fn blob_envelope_round_trips_and_rejects_tampering() {
        let k = key(4);
        let path = "claude-sessions/s/m-000001.jsonl.gz";
        let bytes = b"segment bytes".to_vec();
        let sealed = k.seal_blob(path, &bytes);
        assert_eq!(&sealed[..4], BLOB_MAGIC);
        assert_eq!(k.open_blob(path, &sealed).unwrap(), bytes);
        assert_eq!(
            k.open_blob("claude-sessions/s/m-000002.jsonl.gz", &sealed),
            Err(E2eError::Tampered)
        );
        let mut bad = sealed.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert_eq!(k.open_blob(path, &bad), Err(E2eError::Tampered));
        let mut bad_body = sealed.clone();
        bad_body[BLOB_HEADER_LEN] ^= 1;
        assert_eq!(k.open_blob(path, &bad_body), Err(E2eError::Tampered));
        assert_eq!(
            k.open_blob(path, b"plain gzip"),
            Err(E2eError::NotAnEnvelope)
        );
        assert!(matches!(
            key(5).open_blob(path, &sealed),
            Err(E2eError::OtherKey(_))
        ));
        // Empty blobs work too.
        assert_eq!(k.open_blob(path, &k.seal_blob(path, b"")).unwrap(), b"");
    }

    #[test]
    fn recovery_text_parses_back_to_the_same_key() {
        let k = SyncKey::generate();
        let text = k.recovery_text();
        assert_eq!(
            text.len(),
            RECOVERY_CHARS + RECOVERY_CHARS / RECOVERY_GROUP - 1
        );
        assert!(text
            .chars()
            .all(|c| c == '-' || BASE32.contains(&(c as u8))));
        assert_eq!(SyncKey::parse_recovery(&text).unwrap(), k);
        // Lower case, spaces and line breaks instead of dashes.
        let sloppy = format!(" {}\n", text.to_lowercase().replace('-', " "));
        assert_eq!(SyncKey::parse_recovery(&sloppy).unwrap(), k);
    }

    #[test]
    fn recovery_text_reads_look_alike_digits_as_letters() {
        let k = key(0);
        let text = k.recovery_text();
        let swapped = text.replace('O', "0").replace('I', "1").replace('B', "8");
        assert_eq!(SyncKey::parse_recovery(&swapped).unwrap(), k);
    }

    #[test]
    fn bad_recovery_text_is_refused_with_a_reason() {
        let text = key(7).recovery_text();
        assert_eq!(
            SyncKey::parse_recovery(&text[..20]),
            Err(RecoveryKeyError::WrongLength(17))
        );
        assert_eq!(
            SyncKey::parse_recovery(&format!("{text}!")),
            Err(RecoveryKeyError::BadCharacter('!'))
        );
        // One character changed: the checksum catches it.
        let mut chars: Vec<char> = text.chars().collect();
        chars[3] = if chars[3] == 'A' { 'C' } else { 'A' };
        let typo: String = chars.into_iter().collect();
        assert_eq!(
            SyncKey::parse_recovery(&typo),
            Err(RecoveryKeyError::Checksum)
        );
        assert!(SyncKey::parse_recovery("").is_err());
    }

    #[test]
    fn base32_round_trips_every_length() {
        for len in 0..40 {
            let bytes: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
            assert_eq!(base32_decode(&base32_encode(&bytes)).unwrap(), bytes);
        }
    }

    #[test]
    fn status_follows_the_marker_and_the_saved_key() {
        let k = key(8);
        let meta = k.new_meta("2026-10-09T00:00:00Z");
        assert_eq!(resolve_status(None, None), E2eStatus::Off);
        // A key saved here and no marker: never "off".
        assert_eq!(resolve_status(None, Some(&k)), E2eStatus::MarkerMissing);
        assert!(E2eStatus::MarkerMissing.is_locked());
        assert_eq!(resolve_status(Some(&meta), None), E2eStatus::NeedsKey);
        assert_eq!(
            resolve_status(Some(&meta), Some(&k)),
            E2eStatus::On {
                key_id: k.key_id_hex()
            }
        );
        assert_eq!(
            resolve_status(Some(&meta), Some(&key(9))),
            E2eStatus::WrongKey
        );
        assert_eq!(
            resolve_status(Some(&json!({"mnemosE2e": 2})), Some(&k)),
            E2eStatus::Unsupported
        );
        // Right key id, check value damaged: not this key.
        let mut damaged = meta.clone();
        let mut check = B64.decode(damaged["check"].as_str().unwrap()).unwrap();
        let last = check.len() - 1;
        check[last] ^= 1;
        damaged["check"] = json!(B64.encode(check));
        assert_eq!(
            resolve_status(Some(&damaged), Some(&k)),
            E2eStatus::WrongKey
        );
        assert!(E2eStatus::NeedsKey.is_locked());
        assert!(!E2eStatus::Off.is_locked());
    }

    #[test]
    fn only_personal_data_is_encrypted() {
        for key in [
            "projects",
            "claude-env",
            "claude-memory:p:o/r",
            "claude-session-index",
            "settings",
        ] {
            assert!(is_encryptable_key(key), "{key}");
        }
        for key in [
            "agents-live:mac",
            "twincurrent-build:x",
            "e2e:projects",
            E2E_META_KEY,
        ] {
            assert!(!is_encryptable_key(key), "{key}");
        }
        assert!(is_encryptable_blob("claude-sessions/s/m.meta.json"));
        assert!(!is_encryptable_blob("e2e/claude-sessions/s/m.meta.json"));
    }

    #[test]
    fn seq_is_bound_to_the_state_envelope() {
        let k = key(10);
        let sealed = k.seal_state_seq("claude-settings", &json!({"h": 1}), 42);
        assert_eq!(sealed["seq"], json!(42));
        assert_eq!(
            k.open_state_seq("claude-settings", &sealed).unwrap(),
            (json!({"h": 1}), 42)
        );
        // A server cannot relabel an old copy as a newer one.
        let mut relabeled = sealed.clone();
        relabeled["seq"] = json!(43);
        assert_eq!(
            k.open_state("claude-settings", &relabeled),
            Err(E2eError::Tampered)
        );
        // Nor strip the seq to pass it off as version 1.
        let mut downgraded = sealed.clone();
        downgraded["mnemosE2e"] = json!(1);
        downgraded.as_object_mut().unwrap().remove("seq");
        assert_eq!(
            k.open_state("claude-settings", &downgraded),
            Err(E2eError::Tampered)
        );
        // A version 2 envelope without seq is not an envelope.
        let mut no_seq = sealed.clone();
        no_seq.as_object_mut().unwrap().remove("seq");
        assert_eq!(
            k.open_state("claude-settings", &no_seq),
            Err(E2eError::NotAnEnvelope)
        );
    }

    #[test]
    fn version_1_state_envelopes_still_open_as_seq_0() {
        let k = key(11);
        let aad = associated_data("state", &k.key_id, "claude-md");
        let msg = serde_json::to_vec(&json!("old")).unwrap();
        let (nonce, mut out) = k.seal(&aad, &msg);
        let tag = out.split_off(out.len() - TAG_LEN);
        let v1 = json!({
            "mnemosE2e": 1,
            "kid": k.key_id_hex(),
            "nonce": B64.encode(nonce),
            "ct": B64.encode(out),
            "tag": B64.encode(tag),
        });
        assert_eq!(
            k.open_state_seq("claude-md", &v1).unwrap(),
            (json!("old"), 0)
        );
    }

    #[test]
    fn rollback_is_an_older_seq_beyond_the_tolerance() {
        assert!(!is_rollback(None, 0));
        assert!(!is_rollback(Some(1_000), 1_000));
        assert!(!is_rollback(
            Some(1_000_000),
            1_000_000 - ROLLBACK_TOLERANCE_MS
        ));
        assert!(is_rollback(
            Some(1_000_000),
            1_000_000 - ROLLBACK_TOLERANCE_MS - 1
        ));
        // Version 1 envelopes (seq 0) once a newer one was seen.
        assert!(is_rollback(Some(ROLLBACK_TOLERANCE_MS + 1), 0));
        assert!(!is_rollback(Some(u64::MAX), u64::MAX));
    }

    #[test]
    fn the_migration_window_is_sealed_in_the_marker() {
        let k = key(12);
        let meta = k.new_meta("2026-10-09T00:00:00Z");
        let start = mnemos_ide_core::registry::parse_timestamp("2026-10-09T00:00:00Z")
            .unwrap()
            .timestamp();
        assert_eq!(
            k.migration_until(&meta),
            Some(start + MIGRATION_WINDOW_SECS)
        );
        // Another key cannot read it, and the marker still resolves.
        assert_eq!(key(13).migration_until(&meta), None);
        assert!(matches!(
            resolve_status(Some(&meta), Some(&k)),
            E2eStatus::On { .. }
        ));
        // A server that copies a window from elsewhere or edits it gets none.
        let other = key(13).new_meta("2030-01-01T00:00:00Z");
        let mut forged = k.new_meta_without_migration("2026-10-09T00:00:00Z");
        assert_eq!(k.migration_until(&forged), None);
        forged["migration"] = other["migration"].clone();
        assert_eq!(k.migration_until(&forged), None);
        let mut flipped = meta.clone();
        let mut bytes = B64.decode(flipped["migration"].as_str().unwrap()).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        flipped["migration"] = json!(B64.encode(bytes));
        assert_eq!(k.migration_until(&flipped), None);
        // A window sealed for the marker cannot be passed off as the check.
        let mut swapped = meta.clone();
        swapped["check"] = meta["migration"].clone();
        assert_eq!(k.matches_meta(&swapped), Err(MetaMismatch::WrongKey));
    }

    #[test]
    fn debug_never_prints_key_material() {
        let k = key(0xab);
        let printed = format!("{k:?}");
        assert!(!printed.contains("171, 171"), "{printed}");
        assert!(!printed.contains(&format!("{:?}", k.aead_key)), "{printed}");
        assert!(printed.contains(&k.key_id_hex()), "{printed}");
    }
}
