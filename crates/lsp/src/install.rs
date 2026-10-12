use anyhow::{Context, Result};

#[cfg(all(feature = "local_fs", unix))]
use async_fs::unix::PermissionsExt;

cfg_if::cfg_if! {
    if #[cfg(feature = "local_fs")] {
        use sha2::{Digest, Sha256};
        use std::path::PathBuf;
    }
}

use crate::language_server_candidate::LanguageServerMetadata;

/// Mnemos IDE: one release asset pinned by version and SHA256.
///
/// Upstream resolved `/releases/latest` through the GitHub API at install time and
/// verified a checksum only when the API happened to return one, so whatever GitHub
/// served as "latest" was installed and run. Language servers are now pinned in source:
/// a version bump is a code change that carries the new digests, and the download is
/// refused unless its SHA256 matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinnedAsset {
    /// The release asset's file name, e.g. `rust-analyzer-aarch64-apple-darwin.gz`.
    pub name: &'static str,
    /// Lowercase hex SHA256 of the asset.
    pub sha256: &'static str,
}

/// Builds install metadata for a pinned GitHub release asset. No network access: the
/// URL is GitHub's stable release download URL for `tag`.
pub fn pinned_github_metadata(
    repo_owner: &str,
    repo_name: &str,
    tag: &str,
    asset: PinnedAsset,
) -> LanguageServerMetadata {
    LanguageServerMetadata {
        version: tag.to_owned(),
        url: Some(format!(
            "https://github.com/{repo_owner}/{repo_name}/releases/download/{tag}/{}",
            asset.name
        )),
        digest: Some(asset.sha256.to_owned()),
    }
}

/// Mnemos IDE: checks `bytes` against the pinned lowercase hex SHA256. Installing
/// without a digest is refused.
#[cfg(feature = "local_fs")]
pub(crate) fn verify_sha256(
    bytes: &[u8],
    expected_digest: Option<&str>,
    what: &str,
) -> Result<()> {
    let expected_digest = expected_digest
        .with_context(|| format!("Refusing to install {what}: no pinned SHA256 digest"))?;
    let actual_digest = format!("{:x}", Sha256::digest(bytes));
    if !actual_digest.eq_ignore_ascii_case(expected_digest) {
        anyhow::bail!(
            "SHA256 checksum mismatch for {what}. Expected: {expected_digest}, Got: {actual_digest}"
        );
    }
    log::info!("Checksum verified for {what}");
    Ok(())
}

/// The type of archive for a GitHub release asset.
#[cfg(feature = "local_fs")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetKind {
    /// A gzip-compressed file (e.g., `rust-analyzer-aarch64-apple-darwin.gz`)
    Gz,
    /// A zip archive (e.g., `rust-analyzer-x86_64-pc-windows-msvc.zip`)
    Zip,
}

#[cfg(feature = "local_fs")]
impl AssetKind {
    /// Determines the asset kind from a file name based on its extension.
    pub fn from_filename(filename: &str) -> Option<Self> {
        if filename.ends_with(".gz") && !filename.ends_with(".tar.gz") {
            Some(AssetKind::Gz)
        } else if filename.ends_with(".zip") {
            Some(AssetKind::Zip)
        } else {
            None
        }
    }
}

/// Downloads and installs a language server binary from GitHub.
///
/// This function:
/// 1. Downloads the binary from the provided URL
/// 2. Verifies the SHA256 checksum, refusing to install without one
/// 3. Extracts/decompresses the archive based on its type
/// 4. Makes the binary executable (on Unix systems)
///
/// # Arguments
/// * `client` - The HTTP client to use for downloading
/// * `metadata` - The server metadata containing version, URL, and pinned digest
/// * `server_name` - The name of the server (e.g., "rust-analyzer") used for the destination path
/// * `asset_kind` - The type of archive (Gz or Zip)
/// * `binary_finder` - Optional callback to locate the binary after extraction. When provided,
///   the full archive is extracted (no filter) and this function is called to find the binary.
///   When `None`, a name-based filter is used during extraction and the binary is assumed to be
///   at `{install_dir}/{server_name}`.
///
/// # Returns
/// The path to the installed binary on success.
#[cfg(feature = "local_fs")]
pub async fn install_from_github(
    client: &http_client::Client,
    metadata: &LanguageServerMetadata,
    server_name: &str,
    asset_kind: AssetKind,
    binary_finder: Option<fn(&std::path::Path) -> Option<PathBuf>>,
) -> Result<PathBuf> {
    let url = metadata
        .url
        .as_ref()
        .context("No download URL provided in metadata")?;

    // Create the destination directory: {data_dir}/{server_name}/{version}
    // If it already exists, remove it first to ensure a clean installation
    let install_dir = mnemos_core::paths::data_dir()
        .join(server_name)
        .join(&metadata.version);
    if install_dir.exists() {
        async_fs::remove_dir_all(&install_dir)
            .await
            .with_context(|| {
                format!(
                    "Failed to remove existing install directory: {:?}",
                    install_dir
                )
            })?;
    }
    async_fs::create_dir_all(&install_dir)
        .await
        .with_context(|| format!("Failed to create install directory: {:?}", install_dir))?;

    // The binary name is the server name (with .exe on Windows)
    let binary_name = if cfg!(windows) {
        format!("{server_name}.exe")
    } else {
        server_name.to_string()
    };

    // Download the file
    log::info!("Downloading {server_name} from {url}");
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("Failed to download from {url}"))?;

    if !response.status().is_success() {
        anyhow::bail!(
            "Download failed with status {}: {}",
            response.status(),
            response.text().await.unwrap_or_default()
        );
    }

    let bytes = response
        .bytes()
        .await
        .context("Failed to read response body")?;

    // Mnemos IDE: the checksum is mandatory (see `PinnedAsset`).
    verify_sha256(&bytes, metadata.digest.as_deref(), server_name)?;

    // Extract the archive based on type
    match asset_kind {
        AssetKind::Gz => {
            let binary_path = install_dir.join(&binary_name);
            node_runtime::extract_gz(&bytes, &binary_path).await?;
        }
        AssetKind::Zip => {
            if binary_finder.is_some() {
                // Extract the full archive when using a custom binary finder
                let no_filter: Option<fn(&str) -> bool> = None;
                node_runtime::extract_zip(&bytes, &install_dir, no_filter).await?;
            } else {
                // Use a filter to extract only the specific binary we need
                let binary_name_clone = binary_name.clone();
                node_runtime::extract_zip(
                    &bytes,
                    &install_dir,
                    Some(move |file_name: &str| {
                        file_name.ends_with(&binary_name_clone)
                            || file_name.ends_with(&format!("/{binary_name_clone}"))
                    }),
                )
                .await?;
            }
        }
    }

    // Locate the binary
    let binary_path = if let Some(finder) = binary_finder {
        finder(&install_dir)
            .with_context(|| format!("Failed to locate {server_name} binary after extraction"))?
    } else {
        install_dir.join(&binary_name)
    };

    // Make the binary executable on Unix systems
    #[cfg(unix)]
    {
        let mut perms = async_fs::metadata(&binary_path)
            .await
            .with_context(|| format!("Failed to get metadata for {:?}", binary_path))?
            .permissions();
        perms.set_mode(0o755);
        async_fs::set_permissions(&binary_path, perms)
            .await
            .with_context(|| format!("Failed to set permissions for {:?}", binary_path))?;
    }

    log::info!("Successfully installed {server_name} to {:?}", binary_path);
    Ok(binary_path)
}

#[cfg(all(test, feature = "local_fs"))]
mod tests {
    use super::*;

    #[test]
    fn checksum_is_mandatory_and_must_match() {
        let bytes = b"hello";
        // printf hello | shasum -a 256
        let good = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        assert!(verify_sha256(bytes, Some(good), "x").is_ok());
        assert!(verify_sha256(bytes, Some(&good.to_uppercase()), "x").is_ok());
        assert!(verify_sha256(bytes, None, "x").is_err());
        assert!(verify_sha256(bytes, Some(&good.replace('2', "3")), "x").is_err());
    }

    #[test]
    fn pinned_metadata_points_at_the_release_download() {
        let metadata = pinned_github_metadata(
            "owner",
            "repo",
            "v1",
            PinnedAsset {
                name: "tool.zip",
                sha256: "ab",
            },
        );
        assert_eq!(metadata.version, "v1");
        assert_eq!(
            metadata.url.as_deref(),
            Some("https://github.com/owner/repo/releases/download/v1/tool.zip")
        );
        assert_eq!(metadata.digest.as_deref(), Some("ab"));
    }
}
