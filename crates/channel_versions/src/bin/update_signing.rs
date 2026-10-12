//! Release-side tool for signed update manifests (Mnemos IDE).
//!
//! Used by script/macos/release and script/windows/release.ps1 so both platforms build, sign and
//! check manifests the same way. The signing key is read from the `MNEMOS_UPDATE_SIGNING_KEY`
//! environment variable (`mnemos-update-sec:...`, see `channel_versions::signed`) and is never
//! printed. See RELEASING.md, "Update signing".

use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::exit;

use anyhow::{bail, ensure, Context as _, Result};
use channel_versions::signed::{
    self, AssetDigest, PlatformRelease, PublicKey, ReleasePayload, SecretKey, PAYLOAD_FORMAT,
    SHA256SUMS_CONTEXT,
};
use channel_versions::{ChannelVersions, ParsedVersion};
use chrono::{DateTime, NaiveDateTime, Utc};
use clap::{Parser, Subcommand};

const SIGNING_KEY_ENV: &str = "MNEMOS_UPDATE_SIGNING_KEY";

// `manifest` writes PAYLOAD_FORMAT into the `signed` slot, which installs that read only format 1
// depend on. A format bump has to keep writing a format 1 payload there and put the new format in
// `signed_next` (see channel_versions::signed), or every such install stops updating.
const _: () = assert!(
    PAYLOAD_FORMAT == 1,
    "keep a format 1 payload in `signed` and write the new format to `signed_next`"
);

#[derive(Parser, Debug)]
#[command(about = "Sign and check Mnemos update manifests")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Generate a new signing key. Writes the secret key to a new file (mode 0600 on Unix) and
    /// prints the public key line to add to script/update-signing.pub.
    Keygen {
        /// Where to write the secret key. Must not exist yet.
        #[arg(long)]
        secret_out: PathBuf,
    },
    /// Print the public key of the key in MNEMOS_UPDATE_SIGNING_KEY.
    PublicKey,
    /// Fail unless the key in MNEMOS_UPDATE_SIGNING_KEY matches one of the public keys in a file.
    CheckKey {
        /// A public key file such as script/update-signing.pub.
        #[arg(long)]
        public_keys: PathBuf,
    },
    /// Print `<sha256> <size>` for a file.
    Hash { file: PathBuf },
    /// Write channel_versions.json: the legacy fields plus a signed per-platform payload.
    Manifest {
        /// The version older builds are told about (`stable.version`, and `dev`/`preview`).
        #[arg(long)]
        version: String,
        #[arg(long, default_value = "stable")]
        channel: String,
        /// `<platform>=<asset name>:<sha256>:<size>`, repeatable. Assets with the same platform
        /// are grouped under it. Every platform gets `--version`.
        #[arg(long = "asset")]
        assets: Vec<String>,
        #[command(flatten)]
        hints: UpdateHints,
        #[arg(long)]
        out: PathBuf,
    },
    /// Write a detached signature for a SHA256SUMS file.
    SignSums {
        #[arg(long = "in")]
        input: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Verify a SHA256SUMS file against its detached signature and a public key file.
    VerifySums {
        #[arg(long = "in")]
        input: PathBuf,
        #[arg(long)]
        sig: PathBuf,
        #[arg(long)]
        public_keys: PathBuf,
    },
    /// Verify a manifest against a public key file and print what it signs.
    VerifyManifest {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        public_keys: PathBuf,
        /// Fail unless every signed platform names this version.
        #[arg(long)]
        expect_version: Option<String>,
    },
    /// Print a new version string stamped with the current UTC time, refusing one that does not
    /// sort after every `--previous` version (so a switch from local-time stamps can never make a
    /// release look older than the one before it).
    StampVersion {
        #[arg(long, default_value = "stable")]
        channel: String,
        #[arg(long, default_value = "01")]
        patch: String,
        /// An existing release version (a tag or the live manifest's). Repeatable.
        #[arg(long = "previous")]
        previous: Vec<String>,
    },
}

/// Optional update hints, signed into every platform of the payload (signing-aware clients take
/// them only from there) and repeated in the legacy fields for builds without signing support.
/// The release scripts pass them through from `MNEMOS_UPDATE_BY`, `MNEMOS_SOFT_CUTOFF` and
/// `MNEMOS_LAST_PROMINENT_UPDATE` when those are set.
#[derive(clap::Args, Debug, Default, Clone)]
struct UpdateHints {
    /// RFC 3339 time by which clients should have updated (the warning banner), such as
    /// 2026-11-01T00:00:00Z.
    #[arg(long, value_parser = parse_update_by)]
    update_by: Option<chrono::DateTime<chrono::FixedOffset>>,
    /// The oldest supported version; older clients are told to update.
    #[arg(long, value_parser = parse_release_version)]
    soft_cutoff: Option<String>,
    /// The latest version with a prominent update; older clients show the prominent update UI.
    #[arg(long, value_parser = parse_release_version)]
    last_prominent_update: Option<String>,
}

impl UpdateHints {
    fn apply_to(&self, release: &mut PlatformRelease) {
        release.update_by = self.update_by;
        release.soft_cutoff = self.soft_cutoff.clone();
        release.last_prominent_update = self.last_prominent_update.clone();
    }

    /// The legacy `{ "version": ... }` object, plus whichever hints are set.
    fn legacy_fields(&self, version: &str) -> serde_json::Value {
        let mut legacy = serde_json::json!({ "version": version });
        if let Some(update_by) = self.update_by {
            legacy["update_by"] = serde_json::Value::String(update_by.to_rfc3339());
        }
        if let Some(soft_cutoff) = &self.soft_cutoff {
            legacy["soft_cutoff"] = serde_json::Value::String(soft_cutoff.clone());
        }
        if let Some(last) = &self.last_prominent_update {
            legacy["last_prominent_update"] = serde_json::Value::String(last.clone());
        }
        legacy
    }
}

fn parse_update_by(text: &str) -> Result<chrono::DateTime<chrono::FixedOffset>, String> {
    chrono::DateTime::parse_from_rfc3339(text.trim())
        .map_err(|e| format!("{text:?} is not an RFC 3339 time: {e}"))
}

fn parse_release_version(text: &str) -> Result<String, String> {
    let text = text.trim();
    ParsedVersion::try_from(text)
        .map(|_| text.to_owned())
        .map_err(|e| format!("{text:?} is not a release version: {e:#}"))
}

fn main() {
    if let Err(err) = run(Args::parse()) {
        eprintln!("update_signing: {err:#}");
        exit(1);
    }
}

fn run(args: Args) -> Result<()> {
    match args.command {
        Command::Keygen { secret_out } => keygen(&secret_out),
        Command::PublicKey => {
            println!("{}", signing_key()?.public_key());
            Ok(())
        }
        Command::CheckKey { public_keys } => {
            let key = signing_key()?.public_key();
            let known = read_public_keys(&public_keys)?;
            ensure!(
                known.contains(&key),
                "{SIGNING_KEY_ENV} is key {}, which is not in {}; clients built from this tree would refuse its manifests",
                key.key_id_hex(),
                public_keys.display()
            );
            println!(
                "{SIGNING_KEY_ENV} is key {}, listed in {}",
                key.key_id_hex(),
                public_keys.display()
            );
            Ok(())
        }
        Command::Hash { file } => {
            let digest = signed::sha256_file(&file)?;
            println!("{} {}", digest.sha256, digest.size);
            Ok(())
        }
        Command::Manifest {
            version,
            channel,
            assets,
            hints,
            out,
        } => manifest(&version, &channel, &assets, &hints, &out),
        Command::SignSums { input, out } => {
            let key = signing_key()?;
            let bytes = fs::read(&input).with_context(|| format!("reading {}", input.display()))?;
            fs::write(
                &out,
                signed::sign_detached(&key, SHA256SUMS_CONTEXT, &bytes),
            )
            .with_context(|| format!("writing {}", out.display()))?;
            Ok(())
        }
        Command::VerifySums {
            input,
            sig,
            public_keys,
        } => {
            let keys = read_public_keys(&public_keys)?;
            let bytes = fs::read(&input).with_context(|| format!("reading {}", input.display()))?;
            let line =
                fs::read_to_string(&sig).with_context(|| format!("reading {}", sig.display()))?;
            signed::verify_detached(&keys, SHA256SUMS_CONTEXT, &bytes, &line)?;
            println!(
                "{} is signed by a key in {}",
                input.display(),
                public_keys.display()
            );
            Ok(())
        }
        Command::VerifyManifest {
            manifest,
            public_keys,
            expect_version,
        } => verify_manifest(&manifest, &public_keys, expect_version.as_deref()),
        Command::StampVersion {
            channel,
            patch,
            previous,
        } => {
            println!(
                "{}",
                stamp_version(Utc::now(), &channel, &patch, &previous)?
            );
            Ok(())
        }
    }
}

fn signing_key() -> Result<SecretKey> {
    let text = std::env::var(SIGNING_KEY_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("{SIGNING_KEY_ENV} is not set"))?;
    SecretKey::parse(&text).with_context(|| format!("{SIGNING_KEY_ENV} is not a signing key"))
}

fn read_public_keys(path: &Path) -> Result<Vec<PublicKey>> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let keys = PublicKey::parse_list(&text)?;
    ensure!(!keys.is_empty(), "{} lists no public key", path.display());
    Ok(keys)
}

fn keygen(secret_out: &Path) -> Result<()> {
    let key = SecretKey::generate()?;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(secret_out)
        .with_context(|| format!("creating {} (it must not exist yet)", secret_out.display()))?;
    writeln!(file, "{}", key.to_secret_text())?;
    eprintln!(
        "Wrote the secret key {} to {}. Keep it out of the repository.",
        key.key_id_hex(),
        secret_out.display()
    );
    eprintln!("Add this public key line to script/update-signing.pub:");
    println!("{}", key.public_key());
    Ok(())
}

/// Parses `<platform>=<name>:<sha256>:<size>`.
fn parse_asset(spec: &str) -> Result<(String, String, AssetDigest)> {
    let (platform, rest) = spec
        .split_once('=')
        .with_context(|| format!("--asset {spec} is not <platform>=<name>:<sha256>:<size>"))?;
    let mut parts = rest.rsplitn(3, ':');
    let size = parts.next().unwrap_or_default();
    let sha256 = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    ensure!(
        !platform.is_empty() && !name.is_empty(),
        "--asset {spec} is not <platform>=<name>:<sha256>:<size>"
    );
    ensure!(
        sha256.len() == 64 && sha256.bytes().all(|b| b.is_ascii_hexdigit()),
        "--asset {spec}: {sha256:?} is not a sha256 in hex"
    );
    let size: u64 = size
        .parse()
        .with_context(|| format!("--asset {spec}: {size:?} is not a size in bytes"))?;
    ensure!(size > 0, "--asset {spec}: an asset cannot be empty");
    Ok((
        platform.to_owned(),
        name.to_owned(),
        AssetDigest {
            sha256: sha256.to_ascii_lowercase(),
            size,
        },
    ))
}

fn manifest(
    version: &str,
    channel: &str,
    assets: &[String],
    hints: &UpdateHints,
    out: &Path,
) -> Result<()> {
    ParsedVersion::try_from(version)
        .with_context(|| format!("{version} is not a release version"))?;
    ensure!(!assets.is_empty(), "a manifest needs at least one --asset");
    let key = signing_key()?;
    let text = manifest_text(version, channel, assets, hints, &key)?;
    fs::write(out, text).with_context(|| format!("writing {}", out.display()))?;
    Ok(())
}

/// Builds, signs and checks the manifest text; `manifest` writes it out.
fn manifest_text(
    version: &str,
    channel: &str,
    assets: &[String],
    hints: &UpdateHints,
    key: &SecretKey,
) -> Result<String> {

    let mut platforms: BTreeMap<String, PlatformRelease> = BTreeMap::new();
    for spec in assets {
        let (platform, name, digest) = parse_asset(spec)?;
        let release = platforms
            .entry(platform.clone())
            .or_insert_with(|| {
                let mut release = PlatformRelease::new(version);
                hints.apply_to(&mut release);
                release
            });
        if release.assets.insert(name.clone(), digest).is_some() {
            bail!("{name} is listed twice for {platform}");
        }
    }
    let payload = ReleasePayload {
        format: PAYLOAD_FORMAT,
        channel: channel.to_owned(),
        issued_at: Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        platforms,
    };
    let envelope = signed::sign_payload(key, &payload)?;

    // The legacy fields are what builds without signing support read; they must keep naming the
    // version every one of those builds can download.
    let legacy = hints.legacy_fields(version);
    let manifest = serde_json::json!({
        "dev": legacy,
        "preview": legacy,
        "stable": legacy,
        "signed": envelope,
    });
    // Parse it back as a client would before writing anything.
    let text = serde_json::to_string_pretty(&manifest)? + "\n";
    let parsed: ChannelVersions = serde_json::from_str(&text)?;
    signed::verify_manifest(&[key.public_key()], &parsed)?;
    for (platform, release) in &payload.platforms {
        for (name, digest) in &release.assets {
            eprintln!(
                "signed {platform}: {name} {} ({} bytes)",
                digest.sha256, digest.size
            );
        }
    }
    if let Some(update_by) = hints.update_by {
        eprintln!("signed hint: update_by {}", update_by.to_rfc3339());
    }
    if let Some(soft_cutoff) = &hints.soft_cutoff {
        eprintln!("signed hint: soft_cutoff {soft_cutoff}");
    }
    if let Some(last) = &hints.last_prominent_update {
        eprintln!("signed hint: last_prominent_update {last}");
    }
    Ok(text)
}

fn verify_manifest(
    manifest: &Path,
    public_keys: &Path,
    expect_version: Option<&str>,
) -> Result<()> {
    let keys = read_public_keys(public_keys)?;
    let text =
        fs::read_to_string(manifest).with_context(|| format!("reading {}", manifest.display()))?;
    let parsed: ChannelVersions =
        serde_json::from_str(&text).context("not a channel_versions.json")?;
    let payload = signed::verify_manifest(&keys, &parsed)?;
    ensure!(
        !payload.platforms.is_empty(),
        "the signed payload names no platform"
    );
    for (platform, release) in &payload.platforms {
        if let Some(expected) = expect_version {
            ensure!(
                release.version == expected,
                "{platform} is signed for {}, expected {expected}",
                release.version
            );
        }
        for (name, digest) in &release.assets {
            println!(
                "{platform} {} {name} {} {}",
                release.version, digest.sha256, digest.size
            );
        }
        if let Some(update_by) = release.update_by {
            println!("{platform} update_by {}", update_by.to_rfc3339());
        }
        if let Some(soft_cutoff) = &release.soft_cutoff {
            println!("{platform} soft_cutoff {soft_cutoff}");
        }
        if let Some(last) = &release.last_prominent_update {
            println!("{platform} last_prominent_update {last}");
        }
    }
    println!(
        "signed (format {}) at {}; stable.version {}",
        payload.format,
        payload.issued_at,
        parsed.stable.version_info().version
    );
    Ok(())
}

/// `v0.<yyyy.MM.dd.HH.mm UTC>.<channel>_<patch>`, refused unless it sorts after every previous.
fn stamp_version(
    now: DateTime<Utc>,
    channel: &str,
    patch: &str,
    previous: &[String],
) -> Result<String> {
    ensure!(
        patch.len() == 2 && patch.bytes().all(|b| b.is_ascii_digit()),
        "--patch {patch} is not two digits"
    );
    let version = format!("v0.{}.{channel}_{patch}", now.format("%Y.%m.%d.%H.%M"));
    let parsed = ParsedVersion::try_from(version.as_str())?;
    let newest = previous
        .iter()
        .filter(|v| !v.trim().is_empty())
        .filter_map(|v| {
            ParsedVersion::try_from(v.trim())
                .ok()
                .map(|p| (p, v.trim()))
        })
        .max_by(|a, b| a.0.cmp(&b.0));
    if let Some((newest, newest_text)) = newest {
        if parsed <= newest {
            let wait = version_time(newest_text)
                .map(|then| format!(" Wait until after {then} UTC, or pass an explicit version."))
                .unwrap_or_default();
            bail!(
                "{version} (UTC) would not sort after the existing release {newest_text}, so clients \
                 would treat it as older.{wait}"
            );
        }
    }
    Ok(version)
}

fn version_time(version: &str) -> Option<NaiveDateTime> {
    let stamp = version.strip_prefix("v0.")?.get(..16)?;
    NaiveDateTime::parse_from_str(stamp, "%Y.%m.%d.%H.%M").ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone as _;

    #[test]
    fn stamps_utc_and_refuses_to_go_backwards() {
        let now = Utc.with_ymd_and_hms(2026, 10, 9, 12, 30, 0).unwrap();
        assert_eq!(
            stamp_version(now, "stable", "01", &[]).unwrap(),
            "v0.2026.10.09.12.30.stable_01"
        );
        // A release stamped in Copenhagen summer time (UTC+2) an hour ago sorts after a UTC stamp
        // made now; the switch has to wait until the clock passes it.
        let local_stamped = "v0.2026.10.09.13.30.stable_01".to_owned();
        let err = stamp_version(now, "stable", "01", &[local_stamped]).unwrap_err();
        assert!(format!("{err:#}").contains("2026-10-09 13:30:00"));
        // Older and unparsable previous versions do not block.
        stamp_version(
            now,
            "stable",
            "01",
            &[
                "v0.2026.10.09.10.29.stable_01".to_owned(),
                "nonsense".to_owned(),
            ],
        )
        .unwrap();
        // Same minute needs a higher patch.
        let same = "v0.2026.10.09.12.30.stable_01".to_owned();
        assert!(stamp_version(now, "stable", "01", std::slice::from_ref(&same)).is_err());
        stamp_version(now, "stable", "02", &[same]).unwrap();
    }

    #[test]
    fn signs_the_update_hints_into_every_platform() {
        let key = SecretKey::generate().unwrap();
        let sha = "ab".repeat(32);
        let hints = UpdateHints {
            update_by: Some(parse_update_by("2026-11-01T00:00:00Z").unwrap()),
            soft_cutoff: Some(parse_release_version("v0.2026.10.01.12.00.stable_01").unwrap()),
            last_prominent_update: Some(
                parse_release_version("v0.2026.10.05.12.00.stable_01").unwrap(),
            ),
        };
        let version = "v0.2026.10.09.12.30.stable_01";
        let text = manifest_text(
            version,
            "stable",
            &[
                format!("windows-x86_64=MnemosSetup.exe:{sha}:42"),
                format!("macos-aarch64=Mnemos-arm64.dmg:{sha}:43"),
            ],
            &hints,
            &key,
        )
        .unwrap();
        let parsed: ChannelVersions = serde_json::from_str(&text).unwrap();
        let payload = signed::verify_manifest(&[key.public_key()], &parsed).unwrap();
        assert_eq!(payload.platforms.len(), 2);
        for release in payload.platforms.values() {
            assert_eq!(release.update_by, hints.update_by);
            assert_eq!(release.soft_cutoff, hints.soft_cutoff);
            assert_eq!(release.last_prominent_update, hints.last_prominent_update);
        }
        // Builds without signing support read the same hints from the legacy fields.
        let legacy = parsed.stable.version_info();
        assert_eq!(legacy.update_by, hints.update_by);
        assert_eq!(legacy.soft_cutoff, hints.soft_cutoff);

        // Without hints, neither the payload nor the legacy fields carry any.
        let plain = manifest_text(
            version,
            "stable",
            &[format!("windows-x86_64=MnemosSetup.exe:{sha}:42")],
            &UpdateHints::default(),
            &key,
        )
        .unwrap();
        assert!(!plain.contains("update_by") && !plain.contains("soft_cutoff"));

        assert!(parse_update_by("next tuesday").is_err());
        assert!(parse_release_version("1.2.3").is_err());
    }

    #[test]
    fn parses_asset_specs() {
        let sha = "ab".repeat(32);
        let (platform, name, digest) =
            parse_asset(&format!("windows-x86_64=MnemosSetup.exe:{sha}:42")).unwrap();
        assert_eq!(platform, "windows-x86_64");
        assert_eq!(name, "MnemosSetup.exe");
        assert_eq!(digest.size, 42);
        assert!(parse_asset("windows-x86_64=MnemosSetup.exe:abc:42").is_err());
        assert!(parse_asset(&format!("MnemosSetup.exe:{sha}:42")).is_err());
        assert!(parse_asset(&format!("windows-x86_64=MnemosSetup.exe:{sha}:0")).is_err());
    }
}
