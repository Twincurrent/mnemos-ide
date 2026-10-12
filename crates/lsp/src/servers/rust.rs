use std::path::Path;
use std::sync::Arc;

#[cfg(feature = "local_fs")]
use crate::install::{install_from_github, pinned_github_metadata, AssetKind, PinnedAsset};
use crate::language_server_candidate::{LanguageServerCandidate, LanguageServerMetadata};
use crate::CommandBuilder;
use async_trait::async_trait;

#[cfg_attr(not(feature = "local_fs"), allow(dead_code))]
pub struct RustAnalyzerCandidate {
    client: Arc<http_client::Client>,
}

/// Mnemos IDE: the pinned rust-analyzer release. Bump the tag and every digest together
/// (the `digest` field of each asset in the GitHub release API response).
#[cfg(feature = "local_fs")]
const PINNED_TAG: &str = "2026-10-05";

/// The pinned rust-analyzer asset for the current platform, or an error on a platform
/// rust-analyzer is not installed for.
///
/// Asset names follow the pattern: rust-analyzer-{arch}-{vendor}-{os}.{ext}
/// e.g. rust-analyzer-aarch64-apple-darwin.gz, rust-analyzer-x86_64-unknown-linux-gnu.gz
#[cfg(feature = "local_fs")]
fn pinned_asset() -> anyhow::Result<PinnedAsset> {
    let (name, sha256) = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => (
            "rust-analyzer-aarch64-apple-darwin.gz",
            "64d3f6b8aacd94a93a03298d7988dfa0c747f8a4fcc539a4e59d9c9f920ca62f",
        ),
        ("macos", "x86_64") => (
            "rust-analyzer-x86_64-apple-darwin.gz",
            "32400dfe2b1472fff9689e063dbb80b8c90e0d4f883af49de7abbba3f2e20705",
        ),
        ("linux", "x86_64") => (
            "rust-analyzer-x86_64-unknown-linux-gnu.gz",
            "28070188df63b6f217768040781decc8db43bc9d29b126847acb365575b09bc9",
        ),
        ("linux", "aarch64") => (
            "rust-analyzer-aarch64-unknown-linux-gnu.gz",
            "3974c863acbbd96ef2cc02a7aaa79123935b05f27ffe4fd393fc229522f75cb9",
        ),
        ("windows", "x86_64") => (
            "rust-analyzer-x86_64-pc-windows-msvc.zip",
            "5a9d499aa26d835406cbccfc52f12bce8286733c5bfe62d79f0ed87cae2e64b9",
        ),
        ("windows", "aarch64") => (
            "rust-analyzer-aarch64-pc-windows-msvc.zip",
            "6dd175fe96b321fa28061a935951ddebd8f919b73192ee2e3d06f9bb9c01e89f",
        ),
        (os, arch) => anyhow::bail!("Unsupported platform for rust-analyzer: {os}/{arch}"),
    };
    Ok(PinnedAsset { name, sha256 })
}

#[cfg(feature = "local_fs")]
const SERVER_NAME: &str = "rust-analyzer";

impl RustAnalyzerCandidate {
    pub fn new(client: Arc<http_client::Client>) -> Self {
        Self { client }
    }

    /// Finds the path to an installed rust-analyzer binary in the data directory.
    ///
    /// Returns the path to the first working binary found (verified by running `--help`).
    #[cfg(feature = "local_fs")]
    pub async fn find_installed_binary_in_data_dir() -> Option<std::path::PathBuf> {
        use tokio::process::Command;

        let install_dir = mnemos_core::paths::data_dir().join(SERVER_NAME);
        if !install_dir.exists() {
            return None;
        }

        // Check if any version directory contains a working binary
        let binary_name = if cfg!(windows) {
            format!("{}.exe", SERVER_NAME)
        } else {
            SERVER_NAME.to_string()
        };

        let Ok(entries) = std::fs::read_dir(&install_dir) else {
            return None;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let binary_path = path.join(&binary_name);
                if binary_path.is_file() {
                    // Verify the binary works by running --help
                    let mut cmd = Command::new(&binary_path);
                    cmd.arg("--help");
                    if cmd
                        .output()
                        .await
                        .map(|output| output.status.success())
                        .unwrap_or(false)
                    {
                        return Some(binary_path);
                    }
                }
            }
        }

        None
    }
}

#[async_trait]
#[cfg(feature = "local_fs")]
impl LanguageServerCandidate for RustAnalyzerCandidate {
    async fn should_suggest_for_repo(&self, path: &Path, _executor: &CommandBuilder) -> bool {
        path.join("Cargo.toml").exists()
    }

    async fn is_installed_in_data_dir(&self, _executor: &CommandBuilder) -> bool {
        Self::find_installed_binary_in_data_dir().await.is_some()
    }

    async fn is_installed_on_path(&self, executor: &CommandBuilder) -> bool {
        executor
            .command(SERVER_NAME)
            .arg("--help")
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    async fn install(
        &self,
        metadata: LanguageServerMetadata,
        _executor: &CommandBuilder,
    ) -> anyhow::Result<()> {
        let asset = pinned_asset()?;
        let asset_kind = AssetKind::from_filename(asset.name).ok_or_else(|| {
            anyhow::anyhow!("Unsupported archive format for asset: {}", asset.name)
        })?;
        install_from_github(&self.client, &metadata, SERVER_NAME, asset_kind, None).await?;
        Ok(())
    }

    async fn fetch_latest_server_metadata(&self) -> anyhow::Result<LanguageServerMetadata> {
        // Mnemos IDE: pinned, not the latest GitHub release (see `PinnedAsset`).
        Ok(pinned_github_metadata(
            "rust-lang",
            "rust-analyzer",
            PINNED_TAG,
            pinned_asset()?,
        ))
    }
}

#[async_trait]
#[cfg(not(feature = "local_fs"))]
impl LanguageServerCandidate for RustAnalyzerCandidate {
    async fn should_suggest_for_repo(&self, _path: &Path, _executor: &CommandBuilder) -> bool {
        false
    }

    async fn is_installed_in_data_dir(&self, _executor: &CommandBuilder) -> bool {
        false
    }

    async fn is_installed_on_path(&self, _executor: &CommandBuilder) -> bool {
        false
    }

    async fn install(
        &self,
        _metadata: LanguageServerMetadata,
        _executor: &CommandBuilder,
    ) -> anyhow::Result<()> {
        // Mnemos IDE: an error, not a panic, in builds without local filesystem access.
        anyhow::bail!("Installing language servers needs the local_fs feature")
    }

    async fn fetch_latest_server_metadata(&self) -> anyhow::Result<LanguageServerMetadata> {
        anyhow::bail!("Installing language servers needs the local_fs feature")
    }
}
