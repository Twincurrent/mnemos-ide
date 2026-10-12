//! Signed release manifests.
//!
//! Mnemos IDE: `channel_versions.json` carries a `signed` envelope next to the legacy
//! `dev`/`preview`/`stable` fields. The envelope holds a payload (base64 of the exact JSON bytes
//! that were signed), the id of the key that signed it and an Ed25519 signature. The payload
//! names, per platform (`<os>-<arch>`, see [`current_platform`]), the version to install and the
//! SHA-256 and size of every asset a client may download for it. Clients verify the signature
//! against the public keys baked into the build before they trust any of it, and verify the
//! downloaded asset against the payload before they install it.
//!
//! Old clients ignore the `signed` field and keep reading `stable.version`, so the envelope is
//! purely additive.
//!
//! Payload formats: `signed` always carries a [`PAYLOAD_FORMAT`] 1 payload, for as long as any
//! install that reads only format 1 may still update. A later format ships beside it in
//! `signed_next` (a list), never in its place, and [`verify_manifest`] picks the highest format
//! this build reads among every envelope that verifies. That way a format bump never strands the
//! installs that predate it.
//!
//! Only signed data decides an update. [`PlatformRelease::apply_to`] overwrites every field of a
//! [`crate::VersionInfo`] that affects behaviour (version, rollback, assets, and the
//! `update_by` / `soft_cutoff` / `last_prominent_update` hints) from the verified payload, so the
//! unsigned legacy section cannot change what a signing-aware client does.
//!
//! Keys are plain Ed25519 (the primitive minisign uses), in two text forms:
//!
//! - public: `mnemos-update-pub:` + base64(key id (8 bytes) || public key (32 bytes))
//! - secret: `mnemos-update-sec:` + base64(key id (8 bytes) || seed (32 bytes))
//!
//! Every signature covers a context string followed by the message, so a signature over a
//! manifest can never be replayed as a signature over a `SHA256SUMS` file, or the other way round.

use std::collections::BTreeMap;
use std::io::Read;

use anyhow::{anyhow, bail, ensure, Context as _, Result};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use ring::signature::{Ed25519KeyPair, KeyPair as _, UnparsedPublicKey, ED25519};
use serde::{Deserialize, Serialize};

/// Prefix of a public key in its text form.
pub const PUBLIC_KEY_PREFIX: &str = "mnemos-update-pub:";
/// Prefix of a secret key in its text form.
pub const SECRET_KEY_PREFIX: &str = "mnemos-update-sec:";

/// Signed before the payload of a release manifest.
pub const MANIFEST_CONTEXT: &[u8] = b"mnemos-update-manifest-v1\0";
/// Signed before the bytes of a `SHA256SUMS` file.
pub const SHA256SUMS_CONTEXT: &[u8] = b"mnemos-sha256sums-v1\0";

/// The payload format this code writes and reads.
pub const PAYLOAD_FORMAT: u32 = 1;

const KEY_ID_LEN: usize = 8;
const KEY_LEN: usize = 32;

/// The `signed` field of `channel_versions.json`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SignedEnvelope {
    /// Hex id of the key that signed `payload`.
    pub key_id: String,
    /// Base64 of the signed payload bytes (a JSON [`ReleasePayload`]).
    pub payload: String,
    /// Base64 of the 64-byte Ed25519 signature over [`MANIFEST_CONTEXT`] || payload bytes.
    pub signature: String,
}

/// What a signed manifest says about a release.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ReleasePayload {
    /// Always [`PAYLOAD_FORMAT`] for now; a client refuses a format it does not know.
    pub format: u32,
    /// The release channel this payload is for (`stable`).
    pub channel: String,
    /// When the payload was signed, RFC 3339 in UTC. Informational.
    pub issued_at: String,
    /// The release for each platform, keyed by [`current_platform`] strings. A platform that is
    /// missing has no release, and its clients do not update.
    pub platforms: BTreeMap<String, PlatformRelease>,
}

/// The release for one platform.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PlatformRelease {
    /// The version string, in the `v0.<yyyy.MM.dd.HH.mm>.<channel>_<nn>` form.
    pub version: String,
    /// Whether this version is a deliberate rollback that clients ahead of it should install.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_rollback: Option<bool>,
    /// Every downloadable asset of this version for this platform, by file name.
    pub assets: BTreeMap<String, AssetDigest>,
    /// The time by which clients should have updated (the warning banner). Signed; the unsigned
    /// legacy `update_by` is ignored once a payload verifies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_by: Option<chrono::DateTime<chrono::FixedOffset>>,
    /// The oldest supported version. Signed; replaces the legacy `soft_cutoff`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub soft_cutoff: Option<String>,
    /// The latest version with a prominent update. Signed; replaces the legacy field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_prominent_update: Option<String>,
}

impl PlatformRelease {
    /// A release of `version` with no assets, hints or rollback flag.
    pub fn new(version: impl Into<String>) -> Self {
        Self {
            version: version.into(),
            is_rollback: None,
            assets: BTreeMap::new(),
            update_by: None,
            soft_cutoff: None,
            last_prominent_update: None,
        }
    }

    /// Makes `info` say exactly what this verified release says. Every field that decides or
    /// shapes an update is overwritten, including the hints, which become `None` when the payload
    /// leaves them out: whatever the unsigned legacy section (or its overrides) said is dropped.
    pub fn apply_to(&self, info: &mut crate::VersionInfo) {
        info.version = self.version.clone();
        info.is_rollback = self.is_rollback;
        info.verified_assets = Some(self.assets.clone());
        info.update_by = self.update_by;
        info.soft_cutoff = self.soft_cutoff.clone();
        info.last_prominent_update = self.last_prominent_update.clone();
        // Never downloaded by the app updater, but it must not carry an unsigned version either.
        info.cli_version = None;
        info.version_for_new_users = None;
    }
}

/// The expected bytes of one asset.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AssetDigest {
    /// Lowercase hex SHA-256.
    pub sha256: String,
    /// Size in bytes.
    pub size: u64,
}

/// The platform key of the running build: `<os>-<arch>` as Rust names them, for example
/// `macos-aarch64`, `windows-x86_64` or `linux-x86_64`.
pub fn current_platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// An Ed25519 public key with its id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicKey {
    key_id: [u8; KEY_ID_LEN],
    key: [u8; KEY_LEN],
}

impl PublicKey {
    /// Parses the `mnemos-update-pub:...` text form.
    pub fn parse(text: &str) -> Result<Self> {
        let bytes = decode_prefixed(text.trim(), PUBLIC_KEY_PREFIX)?;
        let (key_id, key) = split_key(&bytes)?;
        Ok(Self { key_id, key })
    }

    /// Parses every non-empty, non-comment (`#`) line of `text` as a public key.
    pub fn parse_list(text: &str) -> Result<Vec<Self>> {
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(Self::parse)
            .collect()
    }

    /// The key id in hex.
    pub fn key_id_hex(&self) -> String {
        hex_encode(&self.key_id)
    }

    /// Verifies `signature` over `context || message`.
    pub fn verify(&self, context: &[u8], message: &[u8], signature: &[u8]) -> Result<()> {
        let mut signed = Vec::with_capacity(context.len() + message.len());
        signed.extend_from_slice(context);
        signed.extend_from_slice(message);
        UnparsedPublicKey::new(&ED25519, &self.key)
            .verify(&signed, signature)
            .map_err(|_| anyhow!("signature does not verify with key {}", self.key_id_hex()))
    }
}

impl std::fmt::Display for PublicKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut bytes = Vec::with_capacity(KEY_ID_LEN + KEY_LEN);
        bytes.extend_from_slice(&self.key_id);
        bytes.extend_from_slice(&self.key);
        write!(f, "{PUBLIC_KEY_PREFIX}{}", BASE64.encode(bytes))
    }
}

/// An Ed25519 signing key with its id. Never logged: it has no `Debug` or `Display`.
pub struct SecretKey {
    key_id: [u8; KEY_ID_LEN],
    pair: Ed25519KeyPair,
    seed: [u8; KEY_LEN],
}

impl SecretKey {
    /// Parses the `mnemos-update-sec:...` text form.
    pub fn parse(text: &str) -> Result<Self> {
        let bytes = decode_prefixed(text.trim(), SECRET_KEY_PREFIX)?;
        let (key_id, seed) = split_key(&bytes)?;
        Self::from_parts(key_id, seed)
    }

    /// Generates a fresh key with a random id.
    pub fn generate() -> Result<Self> {
        use ring::rand::{SecureRandom as _, SystemRandom};
        let rng = SystemRandom::new();
        let mut key_id = [0u8; KEY_ID_LEN];
        let mut seed = [0u8; KEY_LEN];
        rng.fill(&mut key_id)
            .map_err(|_| anyhow!("no system randomness"))?;
        rng.fill(&mut seed)
            .map_err(|_| anyhow!("no system randomness"))?;
        Self::from_parts(key_id, seed)
    }

    fn from_parts(key_id: [u8; KEY_ID_LEN], seed: [u8; KEY_LEN]) -> Result<Self> {
        let pair = Ed25519KeyPair::from_seed_unchecked(&seed)
            .map_err(|_| anyhow!("invalid Ed25519 seed"))?;
        Ok(Self { key_id, pair, seed })
    }

    /// The matching public key.
    pub fn public_key(&self) -> PublicKey {
        let mut key = [0u8; KEY_LEN];
        key.copy_from_slice(self.pair.public_key().as_ref());
        PublicKey {
            key_id: self.key_id,
            key,
        }
    }

    /// The key id in hex.
    pub fn key_id_hex(&self) -> String {
        hex_encode(&self.key_id)
    }

    /// Signs `context || message`.
    pub fn sign(&self, context: &[u8], message: &[u8]) -> Vec<u8> {
        let mut signed = Vec::with_capacity(context.len() + message.len());
        signed.extend_from_slice(context);
        signed.extend_from_slice(message);
        self.pair.sign(&signed).as_ref().to_vec()
    }

    /// The `mnemos-update-sec:...` text form. Only for writing a freshly generated key to a file.
    pub fn to_secret_text(&self) -> String {
        let mut bytes = Vec::with_capacity(KEY_ID_LEN + KEY_LEN);
        bytes.extend_from_slice(&self.key_id);
        bytes.extend_from_slice(&self.seed);
        format!("{SECRET_KEY_PREFIX}{}", BASE64.encode(bytes))
    }
}

/// Signs a payload into an envelope. The payload is serialized once and those exact bytes are
/// what is signed and embedded.
pub fn sign_payload(key: &SecretKey, payload: &ReleasePayload) -> Result<SignedEnvelope> {
    let bytes = serde_json::to_vec(payload).context("serializing the release payload")?;
    let signature = key.sign(MANIFEST_CONTEXT, &bytes);
    Ok(SignedEnvelope {
        key_id: key.key_id_hex(),
        payload: BASE64.encode(&bytes),
        signature: BASE64.encode(signature),
    })
}

/// Verifies an envelope against `keys` and returns its payload. Fails when no key has the
/// envelope's id, the signature does not verify, or the payload format is not
/// [`PAYLOAD_FORMAT`].
pub fn verify_envelope(keys: &[PublicKey], envelope: &SignedEnvelope) -> Result<ReleasePayload> {
    let (format, bytes) = verify_envelope_bytes(keys, envelope)?;
    ensure!(
        format == PAYLOAD_FORMAT,
        "signed manifest has format {format}, this build reads {PAYLOAD_FORMAT}"
    );
    serde_json::from_slice(&bytes).context("signed manifest payload is not valid JSON")
}

/// Verifies every envelope of a manifest (`signed`, then each of `signed_next`) and returns the
/// payload of the highest format this build reads. Envelopes of a format this build does not
/// know are skipped, not fatal, so a manifest can carry a newer format beside format 1. Fails
/// when no envelope both verifies and has a known format.
pub fn verify_manifest(
    keys: &[PublicKey],
    versions: &crate::ChannelVersions,
) -> Result<ReleasePayload> {
    let envelopes: Vec<&SignedEnvelope> = versions
        .signed
        .iter()
        .chain(versions.signed_next.iter())
        .collect();
    ensure!(!envelopes.is_empty(), "the release manifest is not signed");
    let mut best: Option<ReleasePayload> = None;
    let mut errors = Vec::new();
    for envelope in envelopes {
        match verify_envelope_bytes(keys, envelope) {
            Ok((format, bytes)) if format == PAYLOAD_FORMAT => {
                match serde_json::from_slice::<ReleasePayload>(&bytes) {
                    Ok(payload) if best.as_ref().is_none_or(|b| payload.format > b.format) => {
                        best = Some(payload)
                    }
                    Ok(_) => {}
                    Err(err) => errors.push(format!("format {format} payload: {err}")),
                }
            }
            Ok((format, _)) => errors.push(format!(
                "signed manifest has format {format}, this build reads {PAYLOAD_FORMAT}"
            )),
            Err(err) => errors.push(format!("{err:#}")),
        }
    }
    best.ok_or_else(|| {
        anyhow!(
            "no envelope in the manifest verifies with a known format: {}",
            errors.join("; ")
        )
    })
}

/// Checks an envelope's key and signature and returns its payload's `format` and raw bytes.
fn verify_envelope_bytes(keys: &[PublicKey], envelope: &SignedEnvelope) -> Result<(u32, Vec<u8>)> {
    ensure!(!keys.is_empty(), "no update signing keys are configured");
    let key = keys
        .iter()
        .find(|key| key.key_id_hex().eq_ignore_ascii_case(&envelope.key_id))
        .ok_or_else(|| anyhow!("manifest is signed by unknown key {}", envelope.key_id))?;
    let payload = BASE64
        .decode(envelope.payload.as_bytes())
        .context("manifest payload is not base64")?;
    let signature = BASE64
        .decode(envelope.signature.as_bytes())
        .context("manifest signature is not base64")?;
    key.verify(MANIFEST_CONTEXT, &payload, &signature)?;

    #[derive(Deserialize)]
    struct FormatOnly {
        format: u32,
    }
    let FormatOnly { format } =
        serde_json::from_slice(&payload).context("signed manifest payload is not valid JSON")?;
    Ok((format, payload))
}

/// A detached signature line for a file such as `SHA256SUMS`: `<key id hex> <base64 signature>`.
pub fn sign_detached(key: &SecretKey, context: &[u8], message: &[u8]) -> String {
    format!(
        "{} {}\n",
        key.key_id_hex(),
        BASE64.encode(key.sign(context, message))
    )
}

/// Verifies a detached signature line written by [`sign_detached`].
pub fn verify_detached(
    keys: &[PublicKey],
    context: &[u8],
    message: &[u8],
    signature_line: &str,
) -> Result<()> {
    let (key_id, signature) = signature_line
        .trim()
        .split_once(' ')
        .context("signature line is not `<key id> <signature>`")?;
    let key = keys
        .iter()
        .find(|key| key.key_id_hex().eq_ignore_ascii_case(key_id))
        .ok_or_else(|| anyhow!("signed by unknown key {key_id}"))?;
    let signature = BASE64
        .decode(signature.trim().as_bytes())
        .context("signature is not base64")?;
    key.verify(context, message, &signature)
}

/// Streams `reader` through SHA-256 and returns the lowercase hex digest and the byte count.
pub fn sha256_reader(mut reader: impl Read) -> Result<AssetDigest> {
    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = vec![0u8; 1 << 16];
    let mut size = 0u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        context.update(&buffer[..read]);
        size += read as u64;
    }
    Ok(AssetDigest {
        sha256: hex_encode(context.finish().as_ref()),
        size,
    })
}

/// Hashes a file on disk.
pub fn sha256_file(path: &std::path::Path) -> Result<AssetDigest> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("opening {} to hash it", path.display()))?;
    sha256_reader(std::io::BufReader::new(file))
}

/// Checks that `actual` is exactly `expected`.
pub fn check_digest(asset: &str, expected: &AssetDigest, actual: &AssetDigest) -> Result<()> {
    if expected.size != actual.size {
        bail!(
            "{asset} is {} bytes, the signed manifest says {}",
            actual.size,
            expected.size
        );
    }
    if !expected.sha256.eq_ignore_ascii_case(&actual.sha256) {
        bail!(
            "{asset} has sha256 {}, the signed manifest says {}",
            actual.sha256,
            expected.sha256
        );
    }
    Ok(())
}

fn decode_prefixed(text: &str, prefix: &str) -> Result<Vec<u8>> {
    let encoded = text
        .strip_prefix(prefix)
        .ok_or_else(|| anyhow!("key does not start with {prefix}"))?;
    BASE64
        .decode(encoded.as_bytes())
        .context("key is not valid base64")
}

fn split_key(bytes: &[u8]) -> Result<([u8; KEY_ID_LEN], [u8; KEY_LEN])> {
    ensure!(
        bytes.len() == KEY_ID_LEN + KEY_LEN,
        "key is {} bytes, expected {}",
        bytes.len(),
        KEY_ID_LEN + KEY_LEN
    );
    let mut key_id = [0u8; KEY_ID_LEN];
    let mut key = [0u8; KEY_LEN];
    key_id.copy_from_slice(&bytes[..KEY_ID_LEN]);
    key.copy_from_slice(&bytes[KEY_ID_LEN..]);
    Ok((key_id, key))
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

#[cfg(test)]
#[path = "signed_tests.rs"]
mod tests;
